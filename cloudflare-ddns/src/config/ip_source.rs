use crate::addr_helper::{AddrParseError, AddrParseExt, IpType};
use crate::config::{Config, Deserializable};
use crate::non_zero;
use crate::num_cpus::num_cpus;
use crate::retrying_client::RetryingClient;
use anyhow::Result;
use bytes::Bytes;
use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Deserializer as JsonDeserializer;
use simdutf8::basic::Utf8Error;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::{Debug, Formatter, Write};
use std::net::IpAddr;
use std::num::NonZero;
use std::ops::Deref;
use std::sync::Arc;
use thiserror::Error;
use toml::Value;
use toml::map::Map;
use url::Url;

#[derive(Debug, Error)]
pub enum GetIpError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("failed to get IPv6 through native methods: {0}")]
    NativeIpv6GrabError(anyhow::Error),
    #[error("plaintext data contained invalid utf8: {0}")]
    Utf8(#[from] Utf8Error),
    #[error("could not turn into a valid ip: {0}")]
    InvalidIp(#[from] AddrParseError),
    #[error("There is no ip source to get our ip from")]
    NoIpSources,
    #[error("ip source timed out")]
    TimeOut(#[from] tokio::time::error::Elapsed),
}

#[derive(Clone, PartialOrd, PartialEq, Ord, Eq)]
pub struct StrOrBytes(pub Box<[u8]>);

impl<'de> Deserialize<'de> for StrOrBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrBytesVisitor;

        impl<'de> Visitor<'de> for StrBytesVisitor {
            type Value = StrOrBytes;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("something that can be interpreted as bytes")
            }

            #[inline(always)]
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_bytes(v.as_bytes())
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(StrOrBytes(Box::from(v)))
            }

            #[inline(always)]
            fn visit_byte_buf<E>(self, v: Vec<u8>) -> std::result::Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(StrOrBytes(v.into_boxed_slice()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let bytes_hint = seq.size_hint().map_or(2048, |x| x.min(2048));

                let mut vec = Vec::with_capacity(bytes_hint);

                while let Some(byte) = seq.next_element::<u8>()? {
                    vec.push(byte)
                }

                self.visit_byte_buf(vec)
            }
        }

        deserializer.deserialize_any(StrBytesVisitor)
    }
}

impl Debug for StrOrBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match simdutf8::basic::from_utf8(&self.0) {
            Ok(s) => {
                f.write_char('b')?;
                <str as Debug>::fmt(s, f)
            }
            Err(_) => <[u8] as Debug>::fmt(&self.0, f),
        }
    }
}
impl Serialize for StrOrBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match simdutf8::basic::from_utf8(&self.0) {
            Ok(str) => serializer.serialize_str(str),
            Err(_) => serializer.serialize_bytes(&self.0),
        }
    }
}

impl Deref for StrOrBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Debug, PartialOrd, PartialEq, Ord, Eq, Serialize, Deserialize)]
pub enum ProcessStep {
    /// parses the current data as utf-8
    Plaintext,

    /// strips the current data of some leading and trailing bytes
    Strip {
        #[serde(skip_serializing_if = "Option::is_none")]
        prefix: Option<StrOrBytes>,
        #[serde(skip_serializing_if = "Option::is_none")]
        suffix: Option<StrOrBytes>,
    },

    /// parses the current data as a json and extracts the value from
    Json { key: Box<str> },
}

fn get_json_key(json: &[u8], key: &str) -> serde_json::Result<serde_json::Value> {
    let mut deserializer = JsonDeserializer::from_slice(json);

    struct JsonVisitor<'a> {
        key: &'a str,
    }

    impl<'de> Visitor<'de> for JsonVisitor<'_> {
        type Value = serde_json::Value;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            write!(formatter, "a json with a field {}", self.key)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut val = None;

            while let Some((key, value)) = map.next_entry::<String, serde_json::Value>()? {
                if val.is_none() && key == self.key {
                    val = Some(value);
                }
            }

            val.ok_or_else(|| Error::custom(format_args!("missing field `{}`", self.key)))
        }
    }

    deserializer.deserialize_map(JsonVisitor { key })
}

#[derive(Serialize, Deserialize)]
struct ProcessIntermediate<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<IpType>,
    steps: Cow<'a, [ProcessStep]>,
}

#[derive(Clone, Debug, PartialOrd, PartialEq, Ord, Eq)]
struct Process {
    steps: Arc<[ProcessStep]>,
}

impl Process {
    pub fn build(mut steps: Vec<ProcessStep>) -> Process {
        while steps
            .pop_if(|step| matches!(step, ProcessStep::Plaintext))
            .is_some()
        {}

        steps.dedup_by(|x, y| matches!((x, y), (ProcessStep::Plaintext, ProcessStep::Plaintext)));

        steps.retain(|step| {
            !matches!(
                step,
                ProcessStep::Strip {
                    prefix: None,
                    suffix: None,
                }
            )
        });

        Process {
            steps: steps.into(),
        }
    }

    async fn run(&self, mut bytes: Bytes, _cfg: &Config) -> Result<IpAddr, GetIpError> {
        for step in self.steps.iter() {
            use ProcessStep as S;
            match step {
                S::Plaintext => {
                    simdutf8::basic::from_utf8(&bytes)?;
                }

                S::Strip { prefix, suffix } => {
                    if let Some(prefix) = prefix
                        && bytes.starts_with(prefix)
                    {
                        bytes = bytes.split_off(prefix.len());
                    }

                    if let Some(suffix) = suffix
                        && bytes.ends_with(suffix)
                    {
                        bytes.truncate(bytes.len().strict_sub(suffix.len()))
                    }
                }

                S::Json { key } => {
                    let val = match get_json_key(&bytes, key)? {
                        serde_json::Value::String(str) => str,
                        val => format!("{val}"),
                    };
                    bytes = val.into()
                }
            }
        }

        Ok(IpAddr::parse_ascii_bytes(bytes.trim_ascii())?)
    }
}

#[derive(PartialOrd, PartialEq, Ord, Eq)]
pub struct Sources {
    sources: BTreeMap<Url, (Option<IpType>, Process)>,
    pub(crate) concurrent_resolve: NonZero<u8>,
}

impl Sources {
    pub fn from_try_iter<I, Steps, E>(
        iter: I,
        concurrent_resolve: Option<NonZero<u8>>,
    ) -> Result<Self, E>
    where
        I: IntoIterator<Item = Result<(Url, Option<IpType>, Steps), E>>,
        Steps: IntoIterator<Item = ProcessStep>,
    {
        iter.into_iter()
            .map(|res| {
                let (url, ip_type, steps) = res?;
                let process = Process::build(steps.into_iter().collect());
                Ok((url, (ip_type, process)))
            })
            .collect::<Result<BTreeMap<Url, (Option<IpType>, Process)>, E>>()
            .map(|sources| Sources {
                sources,
                concurrent_resolve: concurrent_resolve.unwrap_or_else(|| {
                    // 4 requests per core is a reasonable default
                    num_cpus()
                        .saturating_mul(non_zero!(4))
                        .try_into()
                        // saturating conversion
                        .unwrap_or(NonZero::<u8>::MAX)
                }),
            })
    }

    pub fn from_iter<I, Steps>(iter: I, concurrent_resolve: Option<NonZero<u8>>) -> Self
    where
        I: IntoIterator<Item = (Url, Option<IpType>, Steps)>,
        Steps: IntoIterator<Item = ProcessStep>,
    {
        let Ok(this) = Self::from_try_iter(
            iter.into_iter().map(Ok::<_, Infallible>),
            concurrent_resolve,
        );

        this
    }

    pub fn sources(&self) -> impl Iterator<Item = IpSource> + '_ {
        self.sources
            .iter()
            .map(|(url, val)| (url.clone(), val.clone()))
            .map(|(url, (ip_type, process))| IpSource {
                url,
                ip_type: ip_type.unwrap_or(IpType::Any),
                process,
            })
    }
}

impl Deserializable for Sources {
    async fn deserialize(text: &str) -> Result<Self> {
        let mut value = toml::from_str::<Map<String, Value>>(text)?;

        macro_rules! get_field {
            ($thing: ident: [$($lit:literal),*] => |$key: ident, $val: ident| $fun: expr) => {
                let mut $thing = None;
                for $key in [$($lit),*] {
                    if let Some($val) = value.remove($key) {
                        if $thing.is_some() {
                            anyhow::bail!("fields {:?} collide, you can't have multiple set at the same time", [$($lit),*])
                        }
                        $thing = Some($fun);
                    }
                }
            };
        }

        get_field!(
            concurrent_resolve: ["concurrent-resolve", "concurrent_resolve"] => |key, val|
                NonZero::<u8>::new(val.try_into::<u8>()?)
                    .ok_or_else(|| anyhow::anyhow!("{key} can't be zero"))?
        );

        let this = Self::from_try_iter(
            value.into_iter().map(|(url, v)| {
                let url = Url::parse(url.as_str())?;
                v.try_into::<ProcessIntermediate>()
                    .map(|v| (url, v.r#type, v.steps.into_owned()))
                    .map_err(anyhow::Error::new)
            }),
            concurrent_resolve,
        )?;

        Ok(this)
    }
}

impl Debug for Sources {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.sources.iter().map(|(url, p)| (url.as_str(), p)))
            .entry(&"concurrent-resolve", &self.concurrent_resolve)
            .finish()
    }
}

impl Default for Sources {
    fn default() -> Self {
        Self::from_iter(include!(concat!(env!("OUT_DIR"), "/sources.array")), None)
    }
}

impl Serialize for Sources {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map_serialize = serializer.serialize_map(Some(self.sources.len()))?;

        for (url, &(r#type, ref proc)) in self.sources.iter() {
            map_serialize.serialize_entry(
                url.as_str(),
                &ProcessIntermediate {
                    r#type,
                    steps: Cow::Borrowed(&proc.steps),
                },
            )?
        }

        map_serialize.end()
    }
}

pub struct IpSource {
    url: Url,
    ip_type: IpType,
    process: Process,
}

impl IpSource {
    pub fn ip_type(&self) -> IpType {
        self.ip_type
    }

    pub async fn resolve_ip(
        self,
        client: &RetryingClient,
        cfg: &Config,
    ) -> Result<IpAddr, GetIpError> {
        let bytes = client.get(self.url.clone()).send().await?.bytes().await?;
        self.process.run(bytes, cfg).await
    }
}

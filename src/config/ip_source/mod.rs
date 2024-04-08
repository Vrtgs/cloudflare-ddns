mod wasm;

use crate::config::ip_source::wasm::with_wasm_driver;
use crate::config::Config;
use crate::retrying_client::RetryingClient;
use crate::util::num_cpus;
use anyhow::Result;
use bytes::Bytes;
use futures::task::noop_waker_ref;
use futures::{StreamExt, TryStreamExt};
use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::de::SliceRead;
use serde_json::Deserializer as JsonDeserializer;
use simdutf8::basic::Utf8Error;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::{Debug, Formatter, Write};
use std::future::Future;
use std::io::ErrorKind;
use std::net::{AddrParseError, Ipv4Addr};
use std::num::NonZeroU8;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io;
use toml::map::Map;
use toml::Value;
use url::Url;

#[derive(Debug, Error)]
pub enum GetIpError {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("plaintext data contained invalid utf8: {0}")]
    Utf8(#[from] Utf8Error),
    #[error("custom parser error: {0}")]
    WasmParser(#[from] anyhow::Error),
    #[error("could not turn into a valid ip: {0}")]
    InvalidIp(#[from] AddrParseError),
    #[error("There is no ip source to get our ip from")]
    NoIpSources,
    #[error(
        "Ip source specified a wasm transformation step, but there was no wasm driver specified"
    )]
    NoWasmDriver,
}

#[derive(PartialOrd, PartialEq, Ord, Eq)]
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
                let bytes_hint = seq.size_hint().map(|x| x.min(2048)).unwrap_or(2048);

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

#[derive(Debug, PartialOrd, PartialEq, Ord, Eq, Serialize, Deserialize)]
pub enum ProcessStep {
    /// parses the current data as utf-8
    Plaintext,

    /// strips the current data of some leading and trailing bytes
    Strip {
        prefix: Option<StrOrBytes>,
        suffix: Option<StrOrBytes>,
    },

    /// parses the current data as a json, and extracts the value from
    Json { key: Box<str> },

    /// parses the current data based on a wasm parser
    WasmTransform { module: Box<Path> },
}

fn get_json_key(json: &[u8], key: &str) -> serde_json::Result<serde_json::Value> {
    let mut deserializer = JsonDeserializer::new(SliceRead::new(json));

    struct JsonVisitor<'a> {
        key: &'a str,
    }

    impl<'de, 'a> Visitor<'de> for JsonVisitor<'a> {
        type Value = serde_json::Value;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            write!(formatter, "a json with a field {}", self.key)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut val = None;

            while let Some((key, value)) = map.next_entry::<&str, serde_json::Value>()? {
                if val.is_none() && key == self.key {
                    val = Some(value);
                }
            }

            val.ok_or_else(|| Error::custom(format_args!("missing field `{}`", self.key)))
        }
    }

    deserializer.deserialize_map(JsonVisitor { key })
}

#[derive(Clone, Debug, PartialOrd, PartialEq, Ord, Eq, Serialize, Deserialize)]
struct Process {
    steps: Arc<[ProcessStep]>,
}

impl Process {
    async fn run(&self, mut bytes: Bytes, cfg: &Config) -> Result<Ipv4Addr, GetIpError> {
        use ProcessStep as S;
        for step in &*self.steps {
            match step {
                S::Plaintext => { simdutf8::basic::from_utf8(&bytes)?; },
                S::Strip { prefix, suffix } => {
                    if let Some(prefix) = prefix {
                        if bytes.starts_with(prefix) {
                            bytes = bytes.split_off(prefix.len());
                        }
                    }

                    if let Some(suffix) = suffix {
                        if bytes.ends_with(suffix) {
                            bytes.truncate(bytes.len() - suffix.len())
                        }
                    }
                }
                S::Json { key } => {
                    let val = match get_json_key(&bytes, key)? {
                        serde_json::Value::String(str) => str,
                        val => format!("{val}"),
                    };
                    bytes = val.into()
                }
                S::WasmTransform { module } => {
                    bytes = with_wasm_driver!(async |x in (cfg.wasm_driver_path())| x.run(&**module, bytes).await)
                        .await?.into()
                }
            }
        }

        Ok(Ipv4Addr::parse_ascii(&bytes)?)
    }
}

async fn into_process(mut steps: Vec<ProcessStep>) -> Result<Process, io::Error> {
    while let Some(ProcessStep::Plaintext) = steps.last() {
        steps.pop();
    }

    steps.dedup_by(|x, y| matches!((x, y), (ProcessStep::Plaintext, ProcessStep::Plaintext)));

    let steps = futures::stream::iter(steps)
        .map(|step| async move {
            use ProcessStep as S;
            match step {
                step @ (S::Json { .. } | S::Plaintext) => Some(Ok(step)),
                S::Strip { prefix, suffix } => match (prefix, suffix) {
                    (None, None) => None,
                    (prefix, suffix) => Some(Ok(S::Strip { prefix, suffix })),
                },
                S::WasmTransform { module } => {
                    let step = tokio::fs::canonicalize(module)
                        .await
                        .and_then(|x| match x.to_str() {
                            Some(_) => Ok(x),
                            None => Err(io::Error::new(
                                ErrorKind::InvalidData,
                                "path contains invalid utf-8",
                            )),
                        })
                        .map(PathBuf::into_boxed_path)
                        .map(|module| S::WasmTransform { module });
                    Some(step)
                }
            }
        })
        .buffered(num_cpus().get())
        .filter_map(|x| async move { x })
        .try_collect::<Vec<_>>()
        .await;

    steps.map(|steps| Process {
        steps: steps.into(),
    })
}

#[derive(PartialOrd, PartialEq, Ord, Eq)]
pub struct Sources {
    sources: BTreeMap<Url, Process>,
    pub(crate) driver_path: Box<Path>,
    pub(crate) concurrent_resolve: NonZeroU8,
}

impl Sources {
    pub async fn from_try_iter<I, Url, Steps, E>(
        iter: I,
        driver_path: Option<Box<Path>>,
        concurrent_resolve: Option<NonZeroU8>,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = Result<(Url, Steps), E>>,
        E: Into<anyhow::Error>,
        Url: AsRef<str>,
        Steps: IntoIterator<Item = ProcessStep>,
    {
        futures::stream::iter(iter)
            .map(|res| async move {
                let (url, steps) = res.map_err(Into::into)?;
                Ok((
                    url::Url::parse(url.as_ref())?,
                    into_process(steps.into_iter().collect()).await?,
                ))
            })
            .buffer_unordered(num_cpus().get())
            .try_collect::<BTreeMap<url::Url, Process>>()
            .await
            .map(|sources| Sources {
                sources,
                driver_path: driver_path
                    .unwrap_or_else(|| Box::from(Path::new("./ddns-wasm-runtime.dll"))),
                // safety: 16 is not = to 0, lol
                concurrent_resolve: concurrent_resolve
                    .unwrap_or(unsafe { NonZeroU8::new_unchecked(16) }),
            })
    }

    pub async fn from_iter<I, Url, Steps>(
        iter: I,
        driver_path: Option<Box<Path>>,
        concurrent_resolve: Option<NonZeroU8>,
    ) -> Result<Self>
    where
        I: IntoIterator<Item = (Url, Steps)>,
        Url: AsRef<str>,
        Steps: IntoIterator<Item = ProcessStep>,
    {
        Self::from_try_iter(
            iter.into_iter().map(Ok::<_, Infallible>),
            driver_path,
            concurrent_resolve,
        )
        .await
    }

    pub async fn deserialize_async(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct ProcessIntermediate {
            steps: Vec<ProcessStep>,
        }

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
                NonZeroU8::new(val.try_into::<u8>()?).ok_or_else(|| anyhow::anyhow!("{key} can't be zero"))?
        );

        get_field!(
            driver_path: ["driver-path", "driver_path"] => |key, val| val.try_into::<Box<Path>>()?
        );

        Self::from_try_iter(
            value
                .into_iter()
                .map(|(url, v)| v.try_into::<ProcessIntermediate>().map(|v| (url, v.steps))),
            driver_path,
            concurrent_resolve,
        )
        .await
    }

    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::deserialize_async(&tokio::fs::read_to_string(path).await?).await
    }

    pub fn sources(&self) -> impl Iterator<Item = IpSource> + '_ {
        self.sources
            .iter()
            .map(|(url, process)| (url.clone(), process.clone()))
            .map(|(url, process)| IpSource { url, process })
    }
}

impl Debug for Sources {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.sources.iter().map(|(url, p)| (url.as_str(), p)))
            .entries(
                self.driver_path
                    .deref()
                    .ne(Path::new("./ddns-wasm-runtime.dll"))
                    .then_some(("driver-path", &*self.driver_path)),
            )
            .entries(
                self.concurrent_resolve
                    .get()
                    .ne(&16)
                    .then_some(("concurrent-resolve", self.concurrent_resolve)),
            )
            .finish()
    }
}

impl Default for Sources {
    fn default() -> Self {
        let Poll::Ready(Ok(sources)) = pin!(Self::from_iter(
            include!("../../../default/gen/sources.array"),
            None,
            None
        ))
        .poll(&mut Context::from_waker(noop_waker_ref())) else {
            panic!("bad build artifact")
        };

        sources
    }
}

impl Serialize for Sources {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map_serialize = serializer.serialize_map(Some(self.sources.len()))?;

        for (url, proc) in self.sources.iter() {
            map_serialize.serialize_entry(url.as_str(), proc)?
        }

        map_serialize.end()
    }
}

pub struct IpSource {
    url: Url,
    process: Process,
}

impl IpSource {
    pub async fn resolve_ip(
        self,
        client: &RetryingClient,
        cfg: &Config,
    ) -> Result<Ipv4Addr, GetIpError> {
        let bytes = client.get(self.url).send().await?.bytes().await?;
        self.process.run(bytes, cfg).await
    }
}

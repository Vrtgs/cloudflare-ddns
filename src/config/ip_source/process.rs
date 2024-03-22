use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter, Write};
use std::future::Future;
use std::net::Ipv4Addr;
use std::ops::Deref;
use tokio::io;
use std::path::{Path, PathBuf};
use serde_json::{Deserializer as JsonDeserializer};
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{Error, MapAccess, SeqAccess, Visitor};
use toml::map::Map;
use toml::Value;
use url::Url;
use anyhow::Result;
use bytes::Bytes;
use serde_json::de::SliceRead;
use crate::config::GetIpError;
use crate::config::ip_source::wasm::with_wasm_driver;
use crate::retrying_client::RetryingClient;
use crate::util::num_cpus;


#[derive(Clone)]
struct StrOrBytes(pub Box<[u8]>);

impl<'de> Deserialize<'de> for StrOrBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> where D: Deserializer<'de> {
        struct StrBytesVisitor;
        
        impl<'de> Visitor<'de> for StrBytesVisitor {
            type Value = StrOrBytes;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("something that can be interpreted as bytes")
            }

            #[inline(always)]
            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> where E: Error {
                self.visit_bytes(v.as_bytes())
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E> where E: Error {
                Ok(StrOrBytes(Box::from(v)))
            }

            
            #[inline(always)]
            fn visit_byte_buf<E>(self, v: Vec<u8>) -> std::result::Result<Self::Value, E> where E: Error {
                Ok(StrOrBytes(v.into_boxed_slice()))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let bytes_hint = seq.size_hint()
                    .map(|x| x.min(2048))
                    .unwrap_or(2048);

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
            },
            Err(_) => <[u8] as Debug>::fmt(&self.0, f),
        }
    }
}

impl Serialize for StrOrBytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        match simdutf8::basic::from_utf8(&self.0) {
            Ok(str) => serializer.serialize_str(str),
            Err(_) => serializer.serialize_bytes(&self.0)
        }
    }
}


impl Deref for StrOrBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "ProcessType")]
enum ProcessStep {
    /// parses the current data as utf-8
    Plaintext,

    /// strips the current data of some leading and trailing bytes
    Strip { prefix: Option<StrOrBytes>, suffix: Option<StrOrBytes> },

    /// parses the current data as a json, and extracts the value from
    Json { key: Box<str> },

    /// parses the current data based on a wasm parser
    WasmTransform { module: Box<Path> }
}

#[derive(Debug, Serialize, Deserialize)]
struct Process {
    #[serde(rename = "step")]
    steps: Box<[ProcessStep]>
}

fn get_json_key(json: &[u8], key: &str) -> serde_json::Result<serde_json::Value> {
    let mut deserializer = JsonDeserializer::new(SliceRead::new(json));

    struct JsonVisitor<'a> {
        key: &'a str
    }

    impl<'de> Visitor<'de> for JsonVisitor<'de> {
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


impl Process {
    async fn run(&self, mut bytes: Bytes) -> Result<Ipv4Addr, GetIpError> {
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
                S::WasmTransform { module } => bytes =
                    with_wasm_driver!(async |x| x.run(&**module, bytes).await).await?
                        .into()
            }
        }

        Ok(Ipv4Addr::parse_ascii(&bytes)?)
    }
}

pub struct Sources {
    sources: BTreeMap<Url, Process>
}

#[derive(Deserialize)]
struct ProcessIntermediate {
    #[serde(rename = "step")]
    steps: Vec<ProcessStep>
}

impl ProcessIntermediate {
    async fn into_process(mut self) -> Result<Process, io::Error> {
        while let Some(ProcessStep::Plaintext) = self.steps.last() {
            self.steps.pop();
        }
        
        self.steps.dedup_by(|x, y|{
            use ProcessStep::Plaintext;
            matches!((x, y), (Plaintext, Plaintext))
        });
        
        let steps = futures::stream::iter(self.steps)
            .map(|step| async move {
                use ProcessStep as S;
                match step {
                    step @ (S::Json{..}|S::Plaintext) => Some(Ok(step)),
                    S::Strip { prefix, suffix } => {
                        match (&prefix, &suffix) {
                            (None, None) => None,
                            _ => Some(Ok(S::Strip { prefix, suffix }))
                        }
                    }
                    S::WasmTransform { module } => {
                        let step = tokio::fs::canonicalize(module).await
                            .map(PathBuf::into_boxed_path)
                            .map(|module| S::WasmTransform { module });
                        Some(step)
                    }
                }
            })
            .buffered(num_cpus().get())
            .filter_map(|x| async { x })
            .try_collect::<Vec<_>>().await;

        steps.map(|steps| Process { steps: steps.into() })
    }
}

impl Sources {
    async fn deserialize_async(text: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct SourcesToml {
            #[serde(rename = "source")]
            sources: Map<String, Value>
        }

        let value = toml::from_str::<SourcesToml>(text)?;
        futures::stream::iter(value.sources)
            .map(|(x, v)| async move {Ok((
                Url::parse(&x)?,
                v.try_into::<ProcessIntermediate>()?.into_process().await?
            ))})
            .buffered(num_cpus().get())
            .try_collect::<BTreeMap<Url, Process>>().await
            .map(|sources| Sources { sources })
    }
    
    fn sources(&self) -> impl Iterator<Item=IpSource<'_>> {
        self.sources.iter()
            .map(|(url, process)| IpSource { url, process })
    }
}

pub struct IpSource<'a> {
    pub url: &'a Url,
    pub process: &'a Process
}


impl<'a> IpSource<'a> {
    pub async fn resolve_ip(&self, client: &RetryingClient) -> Result<Ipv4Addr, GetIpError> {
        let bytes = client.get(self.url.clone()).send().await?.bytes().await?;
        self.process.run(bytes).await
    }
}

impl Debug for Sources {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.sources.iter().map(|(url, p)| (url.as_str(), p)))
            .finish()
    }
}
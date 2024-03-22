use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter, Write};
use std::net::Ipv4Addr;
use std::ops::Deref;
use tokio::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use futures::{StreamExt, TryStreamExt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde::de::{Error, SeqAccess, Visitor};
use toml::map::Map;
use toml::Value;
use url::Url;
use anyhow::Result;
use crate::config::GetIpError;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Process {
    #[serde(rename = "step")]
    steps: Arc<[ProcessStep]>
}

#[derive(Clone)]
struct Sources {
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
                use ProcessStep::*;
                match step {
                    step @ (Json{..}|Plaintext) => Some(Ok(step)),
                    Strip { prefix, suffix } => {
                        match (&prefix, &suffix) {
                            (None, None) => None,
                            _ => Some(Ok(Strip { prefix, suffix }))
                        }
                    }
                    WasmTransform { module } => {
                        let step = tokio::fs::canonicalize(module).await
                            .map(PathBuf::into_boxed_path)
                            .map(|module| WasmTransform { module });
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
impl<'de> Sources {
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
}

impl Debug for Sources {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.sources.iter().map(|(url, p)| (url.as_str(), p)))
            .finish()
    }
}

pub struct IpSource {
    pub url: Url,
    pub process: crate::config::ip_source::Process
}


impl IpSource {
    pub fn new(url: Url, process: crate::config::ip_source::Process) -> Self {
        Self { url, process }
    }

    pub async fn resolve_ip(&self, client: &RetryingClient) -> std::result::Result<Ipv4Addr, GetIpError> {
        let bytes = client.get(self.url.clone())
            .send().await?.bytes().await?;

        self.process.run(bytes).await
    }
}
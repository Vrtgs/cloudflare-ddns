use std::fmt::Formatter;
use std::net::{AddrParseError, Ipv4Addr};
use std::sync::Arc;
use bytes::Bytes;
use simdutf8::basic::Utf8Error;
use reqwest::Url;
use serde::de::{Error, MapAccess, Visitor};
use serde::Deserializer;
use serde_json::{Deserializer as JsonDeserializer, Value};
use serde_json::de::SliceRead;
use thiserror::Error;
use crate::config::ip_source::wasm::with_wasm_driver;
use crate::retrying_client::RetryingClient;

mod wasm;
mod process;

#[derive(Clone)]
pub enum ProcessStep {
    /// parses the current data as utf-8
    Plaintext,
    
    /// strips the current data of some leading and trailing bytes
    Strip { prefix: Option<Arc<[u8]>>, suffix: Option<Arc<[u8]>> },
    
    /// parses the current data as a json, and extracts the value from
    Json { key: Arc<str> },
    
    /// parses the current data based on a wasm parser
    WasmTransform { module: Arc<str> }
}

pub struct Process {
    pub steps: Arc<[ProcessStep]>
}

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
    InvalidIp(#[from] AddrParseError)
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
                        Value::String(str) => str,
                        val => format!("{val}"),
                    };

                    bytes = val.into()
                }
                S::WasmTransform { module } => bytes =
                    with_wasm_driver!(async |x| x.run(&**module, bytes).await).await?.into()
            }
        }

        Ok(Ipv4Addr::parse_ascii(&bytes)?)
    }
}

fn get_json_key(json: &[u8], key: &str) -> serde_json::Result<Value> {
    let mut deserializer = JsonDeserializer::new(SliceRead::new(json));

    struct JsonVisitor<'a> {
        key: &'a str
    }

    impl<'de> Visitor<'de> for JsonVisitor<'de> {
        type Value = Value;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            write!(formatter, "a json with a field {}", self.key)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut val = None;

            while let Some((key, value)) = map.next_entry::<&str, Value>()? {
                if val.is_none() && key == self.key {
                    val = Some(value);
                }
            }

            val.ok_or_else(|| Error::custom(format_args!("missing field `{}`", self.key)))
        }
    }

    deserializer.deserialize_map(JsonVisitor { key })
}

pub struct IpSource {
    pub url: Url,
    pub process: Process
}


impl IpSource {
    pub fn new(url: Url, process: Process) -> Self {
        Self { url, process }
    }
    
    pub async fn resolve_ip(&self, client: &RetryingClient) -> Result<Ipv4Addr, GetIpError> {
        let bytes = client.get(self.url.clone())
            .send().await?.bytes().await?;
        
        self.process.run(bytes).await
    }
}
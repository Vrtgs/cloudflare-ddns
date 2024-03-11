use std::borrow::Cow;
use simdutf8::basic::Utf8Error;
use reqwest::Url;
use serde::de::Error as _;
use serde_json::{Map, Value};
use thiserror::Error;
use crate::config::dns_source::wasm::WasmDdnsStep;
use crate::entity::{OwnedBytes, OwnedStr};

mod wasm;


enum ProcessStep {
    /// parses the current data as utf-8
    Plaintext,
    
    /// strips the current data of some leading and trailing bytes
    Strip { prefix: Option<OwnedBytes>, suffix: Option<OwnedBytes> },
    
    /// parses the current data as a json, and extracts the value from
    Json { ip_key: OwnedStr },
    
    /// parses the current data based on a wasm parser
    WasmTransform { step: WasmDdnsStep }
}

struct Process {
    // the array of process is always terminated by ProcessStep::Plaintext, and so we don't have to store that
    steps: Box<[ProcessStep]>
}


#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("plaintext data contained invalid utf8: {0}")]
    Utf8(#[from] Utf8Error),
    #[error("custom parser error: {0}")]
    WasmParser(#[from] anyhow::Error)
}

impl Process {
    async fn run(&self, data: &[u8]) -> Result<Box<str>, Error> {
        let mut bytes = Cow::Borrowed(data);
        for step in &*self.steps {
            match step {
                ProcessStep::Plaintext => if let Err(e) = simdutf8::basic::from_utf8(&bytes) {
                    return Err(Error::Utf8(e))
                },
                ProcessStep::Strip { prefix, suffix } => {
                    fn own(x: &[u8]) -> Cow<'static, [u8]> { x.to_vec().into() }
                    
                    if let Some(prefix) = prefix {
                        bytes = bytes.strip_prefix(&**prefix).map(own).unwrap_or(bytes);
                    }
                    if let Some(suffix) = suffix {
                        bytes = bytes.strip_suffix(&**suffix).map(own).unwrap_or(bytes);
                    }
                }
                ProcessStep::Json { ip_key } => {
                    let mut map = serde_json::from_slice::<Map<String, Value>>(&bytes)?;
                    let val = map
                        .get_mut(&**ip_key)
                        .ok_or_else(|| serde_json::Error::custom(format_args!("Invalid json, missing field: {ip_key}")))?;
                    
                    let val = match val {
                        Value::String(str) => std::mem::take(str),
                        val => format!("{val}"),
                    }; drop(map);
                    
                    bytes = Cow::Owned(val.into_bytes())
                }
                ProcessStep::WasmTransform { step } => bytes = Cow::Owned(step.run(&bytes).await?)
            }
        }
        
        Ok(Box::from(simdutf8::basic::from_utf8(&bytes)?))
    }
}

struct IpSource {
    url: Url,
    process: Process
}
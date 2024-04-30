use crate::config::ip_source::{IpSource, Sources};
use crate::retrying_client::{RequestBuilder, AUTHORIZATION_EMAIL, AUTHORIZATION_KEY};
use anyhow::Result;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use std::num::NonZeroU8;
use std::path::Path;
use std::sync::Arc;

pub mod ip_source;
pub mod listener;

#[derive(Eq, Ord, PartialOrd, PartialEq, Debug)]
enum Auth {
    Token(HeaderValue),
    Key(HeaderValue),
}

#[derive(Eq, Ord, PartialOrd, PartialEq, Debug)]
pub struct Account {
    email: HeaderValue,
    auth: Auth,
}

macro_rules! invalid_header {
            ($field:literal) => {
                Error::custom(
                    concat!($field, " can't be parsed as a valid punycode http header less than or equal to 255 charachters long")
                )
            };
        }

impl<'de> Deserialize<'de> for Account {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct AccountInner {
            email: Box<str>,
            #[serde(alias = "api-token")]
            auth_token: Option<Box<str>>,
            #[serde(alias = "auth-key")]
            auth_key: Option<Box<str>>,
        }
        let inner = AccountInner::deserialize(deserializer)?;

        let email = HeaderValue::from_str(&inner.email).map_err(|_| invalid_header!("email"))?;

        let auth = match (inner.auth_token, inner.auth_key) {
            (Some(token), None) => Auth::Token(
                HeaderValue::from_str(&("Bearer ".to_owned() + &token))
                    .map_err(|_| invalid_header!("auth-token"))?,
            ),
            (None, Some(key)) => {
                Auth::Key(HeaderValue::from_str(&key).map_err(|_| invalid_header!("auth-key"))?)
            }
            (None, None) => return Err(Error::missing_field("auth-token")),
            (Some(_), Some(_)) => return Err(Error::custom("auth-token and auth-key conflict")),
        };

        Ok(Account { auth, email })
    }
}

#[derive(Eq, Ord, PartialOrd, PartialEq, Deserialize, Debug)]
pub struct Zone {
    id: Box<str>,
    record: Box<str>,

    #[serde(default)]
    proxied: bool,
}

impl Zone {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn record(&self) -> &str {
        &self.record
    }

    pub fn proxied(&self) -> bool {
        self.proxied
    }
}

#[derive(Eq, Ord, PartialOrd, PartialEq, Deserialize, Debug)]
pub struct ApiFields {
    account: Account,
    zone: Zone,
}

impl ApiFields {
    pub(crate) fn deserialize(text: &str) -> Result<Self> {
        Ok(toml::de::from_str(text)?)
    }

    pub(crate) async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::deserialize(&tokio::fs::read_to_string(path).await?)
    }
}

#[derive(Debug, Eq, Ord, PartialOrd, PartialEq)]
pub(crate) struct CfgInner {
    api_fields: Arc<ApiFields>,
    ip_sources: Arc<Sources>,
}

impl CfgInner {
    pub(crate) fn new(
        api_fields: impl Into<Arc<ApiFields>>,
        ip_sources: impl Into<Arc<Sources>>,
    ) -> Self {
        Self {
            api_fields: api_fields.into(),
            ip_sources: ip_sources.into(),
        }
    }
}

/// Cheap clone to read-only config
#[derive(Debug, Clone)]
pub struct Config(Arc<CfgInner>);

impl Config {
    pub fn ip_sources(&self) -> impl Iterator<Item = IpSource> + '_ {
        self.0.ip_sources.sources()
    }

    pub fn zone(&self) -> &Zone {
        &self.0.api_fields.zone
    }

    pub fn account(&self) -> &Account {
        &self.0.api_fields.account
    }

    pub fn concurrent_resolve(&self) -> NonZeroU8 {
        self.0.ip_sources.concurrent_resolve
    }

    pub fn wasm_driver_path(&self) -> &Path {
        &self.0.ip_sources.driver_path
    }

    pub fn authorize_request(&self, request: RequestBuilder) -> RequestBuilder {
        let request = request.header(AUTHORIZATION_EMAIL, self.account().email.clone());

        match &self.account().auth {
            Auth::Token(token_header) => request.header(AUTHORIZATION, token_header.clone()),
            Auth::Key(key_header) => request.header(AUTHORIZATION_KEY, key_header.clone()),
        }
    }
}

use crate::config::ip_source::{IpSource, Sources};
use crate::retrying_client::{
    RequestBuilder, AUTHORIZATION_EMAIL, AUTHORIZATION_KEY, AUTH_EMAIL, AUTH_KEY,
};
use std::num::NonZeroU8;
use std::path::Path;
use std::sync::Arc;

pub mod ip_source;
pub mod listener;

#[derive(Eq, Ord, PartialOrd, PartialEq)]
pub(crate) struct CfgMut {
    ip_sources: Sources,
}

/// Cheap clone to read-only config
#[derive(Clone)]
pub struct Config(Arc<CfgMut>);

impl Config {
    pub fn ip_sources(&self) -> impl Iterator<Item = IpSource> + '_ {
        self.0.ip_sources.sources()
    }

    pub fn concurrent_resolve(&self) -> NonZeroU8 {
        const DEFAULT: NonZeroU8 = unsafe { NonZeroU8::new_unchecked(16) };
        self.0.ip_sources.concurrent_resolve.unwrap_or(DEFAULT)
    }

    pub fn wasm_driver_path(&self) -> &Path {
        self.0
            .ip_sources
            .driver_path
            .as_deref()
            .unwrap_or(Path::new("./ddns-wasm-runtime.dll"))
    }

    pub fn authorize_request(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .header(AUTHORIZATION_EMAIL, AUTH_EMAIL)
            .header(AUTHORIZATION_KEY, AUTH_KEY)
    }
}

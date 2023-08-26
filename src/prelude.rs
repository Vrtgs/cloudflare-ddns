use reqwest::header::HeaderName;
use async_trait::async_trait;
use reqwest::Response;
use serde::de::DeserializeOwned;


#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_EMAIL: HeaderName = HeaderName::from_static("x-auth-email");
#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_KEY: HeaderName = HeaderName::from_static("x-auth-key");


#[derive(Debug)]
pub enum JsonError {
    Deserialize(simd_json::Error),
    Reqwest(reqwest::Error)
}

#[async_trait]
pub trait SimdJsonCompat {
    async fn json<T: DeserializeOwned>(self) -> Result<T, JsonError>;
}

#[async_trait]
impl SimdJsonCompat for Response {
    async fn json<T: DeserializeOwned>(self) -> Result<T, JsonError> {
        let mut bytes = self.bytes().await
            .map_err(JsonError::Reqwest)?
            .to_vec();

        simd_json::from_slice(&mut bytes).map_err(JsonError::Deserialize)
    }
}
use reqwest::header::HeaderName;
use reqwest::Response;
use simd_json_derive::Deserialize;
use thiserror::Error;


#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_EMAIL: HeaderName = HeaderName::from_static("x-auth-email");
#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_KEY: HeaderName = HeaderName::from_static("x-auth-key");


#[derive(Debug, Error)]
pub enum JsonError {
    #[error(transparent)]
    Deserialize(#[from] simd_json::Error),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error)
}

pub trait SimdJsonCompat {
    async fn simd_json<T: for<'de> Deserialize<'de>>(self) -> Result<T, JsonError>;
}

impl SimdJsonCompat for Response {
    async fn simd_json<T: for<'de> Deserialize<'de>>(self) -> Result<T, JsonError> {
        let bytes = self.bytes().await?;
        let mut bytes = Vec::from(bytes);

        T::from_slice(&mut bytes).map_err(JsonError::Deserialize)
    }
}
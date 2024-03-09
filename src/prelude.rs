use reqwest::header::HeaderName;

#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_EMAIL: HeaderName = HeaderName::from_static("x-auth-email");
#[allow(clippy::declare_interior_mutable_const)]
pub const AUTHORIZATION_KEY: HeaderName = HeaderName::from_static("x-auth-key");
use std::num::NonZeroU8;
use std::time::Duration;
use reqwest::{Body, Client, ClientBuilder, IntoUrl, Method, Request, Response};
use reqwest::header::{HeaderName, HeaderValue};
use crate::dbg_println;


pub struct RequestBuilder {
    client: RetryingClient,
    req: reqwest::Result<Request>
}

impl RequestBuilder {
    pub fn header(mut self, key: HeaderName, value: HeaderValue) -> RequestBuilder {
        if let Ok(ref mut req) = self.req {
            req.headers_mut().append(key, value);
        }
        self
    }
    pub fn body(mut self, body: impl Into<Body>) -> RequestBuilder {
        if let Ok(ref mut req) = self.req {
            *req.body_mut() = Some(body.into());
        }
        self
    }

    pub async fn send(self) -> reqwest::Result<Response> {
        match self.req {
            Ok(req) => self.client.execute(req).await,
            Err(e) => Err(e)
        }
    }
}

#[derive(Clone)]
pub struct RetryingClient {
    client: Client
}

const MAX_RETRY: NonZeroU8 = match NonZeroU8::new(8) {
    Some(s) => s,
    None => std::panic!("Invalid MAX_RETRY")
};

impl RetryingClient {
    pub fn new() -> Self {
        const TIMEOUT: Duration = Duration::from_secs((2 * 60) + 30); // 2.5 minutes

        #[cfg(feature = "trace")]
        const IDLE_TIMEOUT: Duration = Duration::new(0, 0);

        #[cfg(not(feature = "trace"))]
        const IDLE_TIMEOUT: Duration = match TIMEOUT.checked_mul(MAX_RETRY.get() as u32) {
            Some(x) => x,
            None => panic!("idle timeout too big")
        };
        
        ClientBuilder::new()
            .timeout(TIMEOUT)
            .pool_idle_timeout(IDLE_TIMEOUT)
            .use_rustls_tls()
            .build()
            .map(|c| RetryingClient { client: c })
            .expect("ClientBuilder failed")
    }

    /// See [`Client::get`]
    pub fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// See [`Client::patch`]
    pub fn patch<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    /// See [`Client::request`]
    pub fn request<U: IntoUrl>(&self, method: Method, url: U) -> RequestBuilder {
        RequestBuilder { client: self.clone(), req: url.into_url().map(|url| Request::new(method, url)) }
    }

    /// See [`Client::execute`]
    pub async fn execute(&self, req: Request) -> reqwest::Result<Response> {
        let mut i = 0_u8;
        loop {
            if i >= (MAX_RETRY.get() - 1) { break }

            if let Some(req) = req.try_clone() {
                match self.client.execute(req).await {
                    Ok(resp) => return Ok(resp),
                    Err(_) => tokio::time::sleep(
                        Duration::from_secs(45 * (i/2).max(1) as u64)
                    ).await,
                }
            } else { break dbg_println!("tried to use a streaming request") }

            i += 1
        }

        self.client.execute(req).await
    }
}
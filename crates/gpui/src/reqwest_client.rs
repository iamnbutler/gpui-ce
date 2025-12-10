//! Lightweight reqwest-based HTTP client for gpui applications.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::anyhow;
use futures::TryStreamExt;
use futures::future::BoxFuture;
use http_client::{AsyncBody, HttpClient, Inner, Request, Response, Url, http};
use reqwest::header::{HeaderMap, HeaderValue};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// A reqwest-based HTTP client implementation.
///
/// This client manages its own tokio runtime if one is not already available,
/// making it suitable for use in non-tokio async environments like smol.
pub struct ReqwestHttpClient {
    client: reqwest::Client,
    handle: tokio::runtime::Handle,
    user_agent: Option<HeaderValue>,
}

impl ReqwestHttpClient {
    /// Create a new HTTP client with the default user agent.
    pub fn new() -> anyhow::Result<Self> {
        Self::with_user_agent("gpui-client")
    }

    /// Create a new HTTP client with a custom user agent.
    pub fn with_user_agent(agent: &str) -> anyhow::Result<Self> {
        let user_agent = HeaderValue::from_str(agent)?;

        let client = reqwest::Client::builder()
            .user_agent(agent)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(60))
            .build()?;

        let handle = tokio::runtime::Handle::try_current().unwrap_or_else(|_| {
            let runtime = RUNTIME.get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .expect("Failed to initialize HTTP client runtime")
            });

            runtime.handle().clone()
        });

        Ok(Self {
            client,
            handle,
            user_agent: Some(user_agent),
        })
    }
}

impl HttpClient for ReqwestHttpClient {
    fn type_name(&self) -> &'static str {
        "ReqwestHttpClient"
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        self.user_agent.as_ref()
    }

    fn send(
        &self,
        req: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let (parts, body) = req.into_parts();

        let mut request = self.client.request(parts.method, parts.uri.to_string());

        let mut headers = HeaderMap::new();
        for (name, value) in parts.headers.iter() {
            headers.insert(name.clone(), value.clone());
        }
        request = request.headers(headers);

        let body_bytes = match body.0 {
            Inner::Empty => bytes::Bytes::new(),
            Inner::Bytes(cursor) => cursor.into_inner(),
            Inner::AsyncReader(mut reader) => {
                use smol::io::AsyncReadExt;
                let mut buffer = Vec::new();
                let handle = self.handle.clone();

                let result = handle.block_on(async { reader.read_to_end(&mut buffer).await });

                match result {
                    Ok(_) => bytes::Bytes::from(buffer),
                    Err(_) => bytes::Bytes::new(),
                }
            }
        };
        request = request.body(body_bytes);

        let handle = self.handle.clone();

        Box::pin(async move {
            let response = handle.spawn(async move { request.send().await }).await??;

            let status = response.status();
            let headers = response.headers().clone();

            let stream = response.bytes_stream().map_err(std::io::Error::other);
            let body_reader = stream.into_async_read();
            let async_body = AsyncBody::from_reader(body_reader);

            let mut builder = http::response::Builder::new().status(status.as_u16());

            for (name, value) in headers.iter() {
                builder = builder.header(name.as_str(), value.as_bytes());
            }

            builder
                .body(async_body)
                .map_err(|e: http::Error| anyhow!(e))
        })
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new().expect("Failed to create default HTTP client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = ReqwestHttpClient::new();
        assert!(client.is_ok());

        let client = client.unwrap();
        assert_eq!(client.type_name(), "ReqwestHttpClient");
    }

    #[test]
    fn test_client_with_user_agent() {
        let client = ReqwestHttpClient::with_user_agent("test-agent/1.0");
        assert!(client.is_ok());

        let client = client.unwrap();
        let user_agent = client.user_agent();
        assert!(user_agent.is_some());
        assert_eq!(user_agent.unwrap().to_str().unwrap(), "test-agent/1.0");
    }

    #[test]
    fn test_proxy_returns_none() {
        let client = ReqwestHttpClient::new().unwrap();
        assert!(client.proxy().is_none());
    }
}

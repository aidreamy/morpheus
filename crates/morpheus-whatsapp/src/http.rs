//! HTTP seam for the protocol engine.
//!
//! whatsapp-rust uses HTTP for two things: fetching the current WhatsApp Web
//! version and moving media. Its bundled client is `ureq`; this crate plugs in
//! `reqwest` instead - already the project's HTTP client and TLS stack - so no
//! second client is built or linked. The engine exposes [`HttpClient`] as a
//! seam precisely for this.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use whatsapp_rust::http::{HttpClient, HttpRequest, HttpResponse};

/// Time spent establishing a TCP/TLS connection before giving up.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Time without receiving bytes before the request is abandoned.
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Overall ceiling, so a slow trickle of bytes cannot hang the caller.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

/// `reqwest`-backed [`HttpClient`] for the engine.
#[derive(Clone, Debug)]
pub(crate) struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    /// Build the client once, with bounded timeouts.
    ///
    /// The engine calls this from its protocol loop (version lookups, media
    /// upload/download), so a half-open socket must fail instead of pinning
    /// the connection indefinitely; async `reqwest` has no timeouts by
    /// default.
    ///
    /// Responses are returned buffered (`HttpResponse` is `{status, body}`),
    /// which is fine for version lookups and small media but means the whole
    /// body is held in memory. Streaming would require widening the engine's
    /// seam, so it is left for when media sending is added.
    pub(crate) fn new() -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn execute(&self, request: HttpRequest) -> anyhow::Result<HttpResponse> {
        let method = Method::from_bytes(request.method.as_bytes())?;
        let mut builder = self.client.request(method, request.url);

        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await?;
        let status_code = response.status().as_u16();
        let body = response.bytes().await?.to_vec();

        Ok(HttpResponse { status_code, body })
    }
}

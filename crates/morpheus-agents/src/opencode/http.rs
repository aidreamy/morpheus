//! Minimal HTTP/SSE client for the OpenCode server.
//!
//! The server is a local process over plain HTTP, so this layer stays
//! deliberately small: a shared [`reqwest::Client`], the base URL, optional
//! HTTP basic auth (the server's `OPENCODE_SERVER_PASSWORD` mode) and a
//! buffered SSE decoder. Everything above it speaks typed values, never
//! `reqwest` types.

use std::collections::VecDeque;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A request against one OpenCode server.
#[derive(Debug, Clone)]
pub struct Http {
    client: reqwest::Client,
    base: String,
    auth: Option<(String, String)>,
    /// Extra header sent on every request (Kilo's `x-kilo-directory` instance
    /// routing). `None` for OpenCode, whose server binds to one directory.
    instance_header: Option<(String, String)>,
}

impl Http {
    /// Build a client for `base` (e.g. `http://127.0.0.1:4096`).
    ///
    /// # Panics
    ///
    /// Panics only if the TLS backend cannot be initialized, which is a
    /// process-wide setup failure.
    #[must_use]
    pub fn new(
        base: impl Into<String>,
        auth: Option<(String, String)>,
        instance_header: Option<(String, String)>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|error| {
                log::error!("opencode: failed to build the HTTP client: {error}");
                reqwest::Client::new()
            });
        Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
            auth,
            instance_header,
        }
    }

    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{path}", self.base);
        let mut request = self.client.request(method, url);
        if let Some((user, password)) = &self.auth {
            request = request.basic_auth(user, Some(password));
        }
        if let Some((name, value)) = &self.instance_header {
            request = request.header(name, value);
        }
        request
    }

    /// `GET path` → typed JSON.
    ///
    /// # Errors
    ///
    /// Returns a readable message when the request fails or the server
    /// answers with a non-success status.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self
            .request(reqwest::Method::GET, path)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("GET {path}: {error}"))?;
        Self::json(response, "GET", path).await
    }

    /// `GET path` → JSON, with `404` mapped to `None` (the "this id no
    /// longer exists" probe used before resuming a session).
    ///
    /// # Errors
    ///
    /// Returns a readable message for every other failure.
    pub async fn get_optional<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, String> {
        let response = self
            .request(reqwest::Method::GET, path)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("GET {path}: {error}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::json(response, "GET", path).await.map(Some)
    }

    /// `POST path` with a JSON body → typed JSON response.
    ///
    /// # Errors
    ///
    /// Returns a readable message when the request fails, the body cannot be
    /// encoded, or the server answers with a non-success status.
    pub async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        let response = self
            .request(reqwest::Method::POST, path)
            .json(body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("POST {path}: {error}"))?;
        Self::json(response, "POST", path).await
    }

    /// `POST path` with a JSON body, discarding the response body (`204`,
    /// `boolean` acknowledgements).
    ///
    /// # Errors
    ///
    /// Returns a readable message when the request fails or the status is
    /// not a success.
    pub async fn post_ack<B: Serialize>(&self, path: &str, body: &B) -> Result<(), String> {
        Self::post_request(
            path,
            self.request(reqwest::Method::POST, path)
                .json(body)
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    /// The command endpoint holds its response open until the turn finishes.
    pub async fn post_command<B: Serialize>(&self, path: &str, body: &B) -> Result<(), String> {
        Self::post_request(path, self.request(reqwest::Method::POST, path).json(body)).await
    }

    pub async fn post_empty(&self, path: &str) -> Result<(), String> {
        Self::post_request(
            path,
            self.request(reqwest::Method::POST, path)
                .timeout(REQUEST_TIMEOUT),
        )
        .await
    }

    async fn post_request(path: &str, request: reqwest::RequestBuilder) -> Result<(), String> {
        let response = request
            .send()
            .await
            .map_err(|error| format!("POST {path}: {error}"))?;
        Self::check_status(response, "POST", path).await.map(drop)
    }

    /// Open the server-sent event stream (`GET /event`).
    ///
    /// # Errors
    ///
    /// Returns a readable message when the connection fails or the server
    /// refuses it.
    pub(super) async fn open_stream(&self, path: &str) -> Result<EventStream, String> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let response = self
                .request(reqwest::Method::GET, path)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .send()
                .await
                .map_err(|error| format!("GET {path} (SSE): {error}"))?;
            Ok(EventStream::new(
                Self::check_status(response, "GET", path).await?,
            ))
        })
        .await
        .map_err(|_| format!("GET {path} (SSE): timed out opening the stream"))?
    }

    async fn json<T: DeserializeOwned>(
        response: reqwest::Response,
        method: &str,
        path: &str,
    ) -> Result<T, String> {
        Self::check_status(response, method, path)
            .await?
            .json::<T>()
            .await
            .map_err(|error| format!("invalid JSON from {path}: {error}"))
    }

    async fn check_status(
        response: reqwest::Response,
        method: &str,
        path: &str,
    ) -> Result<reqwest::Response, String> {
        let status = response.status();
        if !status.is_success() {
            let body = tokio::time::timeout(REQUEST_TIMEOUT, response.text())
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
            return Err(format!(
                "{method} {path} failed with HTTP {status}: {}",
                body.trim()
            ));
        }
        Ok(response)
    }
}

pub(super) struct EventStream {
    response: reqwest::Response,
    decoder: SseDecoder,
    pending: VecDeque<serde_json::Value>,
}

impl EventStream {
    fn new(response: reqwest::Response) -> Self {
        Self {
            response,
            decoder: SseDecoder::default(),
            pending: VecDeque::new(),
        }
    }

    pub(super) async fn next(&mut self) -> Result<Option<serde_json::Value>, String> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            let Some(chunk) =
                self.response.chunk().await.map_err(|error| {
                    format!("opencode: failed to read the event stream: {error}")
                })?
            else {
                return Ok(None);
            };
            self.pending.extend(self.decoder.push(&chunk));
        }
    }
}

/// Buffered decoder for the server's SSE stream.
///
/// The stream is `data: {json}` frames separated by a blank line (the server
/// also sends comment lines such as `: ping`, and keeps connections open
/// with no payload). Chunks arrive at arbitrary byte boundaries, so the
/// decoder only ever parses COMPLETE frames and keeps the remainder buffered
/// - a multi-byte UTF-8 sequence split across two chunks is therefore never
/// decoded as a replacement character.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    /// Feed one network chunk, returning every complete JSON payload it
    /// completed.
    ///
    /// Payloads that are not valid JSON are logged and dropped: a malformed
    /// frame must not kill the turn, because the event union is versioned by
    /// the server and this client is intentionally tolerant.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<serde_json::Value> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        // Frames are separated by a blank line; accept both `\n\n` and
        // `\r\n\r\n` spellings.
        while let Some((end, skip)) = frame_boundary(&self.buffer) {
            let frame: Vec<u8> = self.buffer.drain(..end).collect();
            // Drop the separator itself.
            self.buffer.drain(..skip);
            let text = String::from_utf8_lossy(&frame);
            let data = sse_data(&text);
            if data.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(&data) {
                Ok(value) => events.push(value),
                Err(error) => log::debug!("opencode: unparsable SSE frame ({error}): {data}"),
            }
        }
        events
    }

    /// Whether a partial frame is still buffered (diagnostics only).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

/// Byte offset where the next complete frame ends, plus the separator length
/// to drop after it.
fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Join the `data:` lines of one frame (the SSE spec allows several).
fn sse_data(frame: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in frame.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(data) = line.strip_prefix("data:") {
            lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_frames_are_decoded_and_the_rest_buffered() {
        let mut decoder = SseDecoder::default();
        let first = decoder.push(b"data: {\"type\":\"server.connected\"}\n\ndata: {\"type\":\"ses");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["type"], "server.connected");
        assert!(!decoder.is_empty());

        let second = decoder.push(b"sion.idle\"}\n\n");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["type"], "session.idle");
        assert!(decoder.is_empty());
    }

    #[test]
    fn crlf_frames_and_comments_are_handled() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b": ping\r\n\r\ndata: {\"ok\":true}\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["ok"], true);
    }

    #[test]
    fn multi_byte_characters_split_across_chunks_survive() {
        let mut decoder = SseDecoder::default();
        let frame = "data: {\"text\":\"olá\"}\n\n".as_bytes();
        // Split INSIDE the two-byte `á`.
        let split = frame
            .windows(2)
            .position(|w| w == "á".as_bytes())
            .expect("the accent byte pair")
            + 1;
        assert!(decoder.push(&frame[..split]).is_empty());
        let events = decoder.push(&frame[split..]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["text"], "olá");
    }

    #[test]
    fn malformed_frames_do_not_kill_the_stream() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(b"data: not json\n\ndata: {\"type\":\"session.idle\"}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "session.idle");
    }
}

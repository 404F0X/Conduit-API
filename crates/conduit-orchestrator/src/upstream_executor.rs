//! Production [`Executor`] that sends a transformed [`HttpRequest`] to the
//! upstream provider over a real `reqwest` client.
//!
//! This is the I/O primitive that was missing while the pipeline's `Executor`
//! trait had only test stubs (`StubExecutor`/`CapturingExecutor`). With it, the
//! orchestrator can actually dial a provider — unblocking the real
//! `/v1/chat/completions` path. The `reqwest::Client` is built upstream by
//! `conduit_llm::HttpClientBuilder` (timeouts/proxy match Go's `httpclient`);
//! this module only turns an `HttpRequest` into a reqwest call and the response
//! back into [`HttpResponse`] / [`StreamEvent`]s.
//!
//! Streaming note: `execute_stream` materializes the full SSE body then parses
//! it into `Vec<StreamEvent>` (the buffered pipeline contract). RUST-P8-003
//! phase 2 adds `execute_stream_live`, which reads the `reqwest` response
//! chunk-by-chunk and forwards SSE frames incrementally; the orchestrator wraps
//! that receiver with the `outbound_stream` forward-while-aggregating loop.

use std::sync::Arc;

use async_trait::async_trait;

use conduit_core::ConduitError;
use conduit_llm::{
    DecodedBody, HttpClientBuilder, HttpRequest, HttpResponse, MAX_ERROR_BODY_BYTES, ProxyConfig,
    StreamEvent, decode_response_body,
};
use conduit_pipeline::{Executor, LiveUpstreamResponse};
use reqwest::header::{HeaderMap as ReqwestHeaderMap, HeaderName, HeaderValue};

/// Production executor backed by a `reqwest::Client`.
pub struct UpstreamExecutor {
    client: reqwest::Client,
    insecure_skip_verify: bool,
}

impl UpstreamExecutor {
    /// Wrap a pre-built `reqwest::Client` (typically from
    /// `conduit_llm::HttpClientBuilder::build`).
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            insecure_skip_verify: false,
        }
    }

    /// Apply the process-level TLS verification policy to clients rebuilt for
    /// per-channel proxy settings. The base client is configured separately by
    /// the production wiring before it is passed to [`Self::new`].
    pub fn with_insecure_skip_verify(mut self, insecure_skip_verify: bool) -> Self {
        self.insecure_skip_verify = insecure_skip_verify;
        self
    }

    /// Box as `Arc` for wiring into the pipeline (`Arc<Pipeline>` holds it).
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn client_for_request(&self, request: &HttpRequest) -> Result<reqwest::Client, ConduitError> {
        let Some(proxy) = proxy_config_from_request(request)? else {
            return Ok(self.client.clone());
        };
        HttpClientBuilder::new()
            .insecure_skip_verify(self.insecure_skip_verify)
            .proxy(proxy)
            .build()
            .map_err(|error| {
                ConduitError::internal("failed to build channel proxy client").with_source(error)
            })
    }
}

fn proxy_config_from_request(request: &HttpRequest) -> Result<Option<ProxyConfig>, ConduitError> {
    let Some(value) = request.metadata.get("channel_proxy") else {
        return Ok(None);
    };
    let parsed = match value {
        serde_json::Value::String(raw) => serde_json::from_str(raw),
        value => serde_json::from_value(value.clone()),
    }
    .map_err(|error| {
        ConduitError::internal("invalid channel proxy configuration").with_source(error)
    })?;
    Ok(Some(parsed))
}

#[async_trait]
impl Executor for UpstreamExecutor {
    async fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
        let client = self.client_for_request(request)?;
        let response = send(&client, request).await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let content_type = header_string(&headers, reqwest::header::CONTENT_TYPE);
        let bytes = response.bytes().await.map_err(|err| {
            ConduitError::upstream(format!("failed to read upstream body: {err}"))
        })?;

        let mut http_response = HttpResponse {
            status,
            headers: header_map_to_llm(&headers),
            ..HttpResponse::default()
        };

        // Decode by content-type so callers get a typed body: JSON → json_body,
        // SSE → stream frames (plus raw bytes), binary → raw bytes. A decode
        // failure falls back to raw bytes rather than dropping the response.
        match decode_response_body(content_type.as_deref(), &bytes) {
            Ok(DecodedBody::Json(value)) => http_response.json_body = Some(value),
            Ok(DecodedBody::Sse(parsed)) => {
                http_response.stream = parsed.frames.into_iter().map(StreamEvent::from).collect();
                http_response.body = Some(bytes.to_vec());
            }
            Ok(DecodedBody::Binary(_)) | Err(_) => {
                http_response.body = Some(bytes.to_vec());
            }
        }
        Ok(http_response)
    }

    async fn execute_stream(
        &self,
        request: &HttpRequest,
    ) -> Result<Vec<StreamEvent>, ConduitError> {
        let client = self.client_for_request(request)?;
        let response = validate_sse_response(send(&client, request).await?).await?;
        let content_type = header_string(response.headers(), reqwest::header::CONTENT_TYPE);
        let bytes = response.bytes().await.map_err(|err| {
            ConduitError::upstream(format!("failed to read upstream stream: {err}"))
        })?;

        match decode_response_body(content_type.as_deref(), &bytes) {
            Ok(DecodedBody::Sse(parsed)) => {
                Ok(parsed.frames.into_iter().map(StreamEvent::from).collect())
            }
            _ => Err(ConduitError::upstream(
                "failed to decode upstream text/event-stream body",
            )),
        }
    }

    /// RUST-P8-003 (phase 2) — the **real** live streaming executor.
    ///
    /// Overrides the trait's buffered fallback: dials the provider, then reads
    /// the response body **chunk-by-chunk** via `reqwest::Response::chunk()`,
    /// parsing SSE frames incrementally with the shared
    /// [`conduit_llm::http::parse_sse_frames`] parser (the `incomplete` tail is
    /// carried across reads, so a frame split across two TCP chunks is not
    /// dropped). Each decoded frame is forwarded to the channel as it arrives —
    /// this is what makes the client see tokens streaming rather than one
    /// materialized blob.
    ///
    /// The provider connection is established **eagerly** (before returning the
    /// receiver) so a connect/handshake error surfaces synchronously, mirroring
    /// Go `executor.DoStream` returning the error up front. The read loop then
    /// runs in a spawned task feeding the channel.
    ///
    /// Cancellation (Go `cancelOnCloseStream` → stream-ctx cancel): the loop
    /// checks `cancel.is_canceled()` before each read and stops on a fired
    /// token; dropping the owned `reqwest::Response` aborts the in-flight
    /// upstream HTTP request. A dropped client receiver (`tx.send` error) also
    /// tears the loop down.
    async fn execute_stream_live(
        &self,
        request: &HttpRequest,
        cancel: conduit_pipeline::CancelToken,
    ) -> Result<LiveUpstreamResponse, ConduitError> {
        // Eagerly establish the connection so connect errors are synchronous.
        let client = self.client_for_request(request)?;
        let binary_speech = is_binary_speech_request(request);
        let mut response = if binary_speech {
            validate_binary_response(send(&client, request).await?).await?
        } else {
            validate_sse_response(send(&client, request).await?).await?
        };
        let content_type = header_string(response.headers(), reqwest::header::CONTENT_TYPE);
        // A single queued binary chunk keeps the reqwest reader tightly
        // coupled to downstream demand. The SSE path retains its established
        // window because those frames are small text records.
        let capacity = if binary_speech { 1 } else { 64 };
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ConduitError>>(capacity);

        if binary_speech {
            let event_content_type = content_type.clone();
            tokio::spawn(async move {
                loop {
                    // Reserve downstream capacity *before* reading the next
                    // provider chunk. This is the backpressure boundary: when
                    // Axum stops polling, no additional reqwest body bytes are
                    // pulled into application memory.
                    let permit = tokio::select! {
                        _ = cancel.cancelled() => break,
                        permit = tx.reserve() => match permit {
                            Ok(permit) => permit,
                            Err(_) => {
                                cancel.cancel();
                                break;
                            },
                        },
                    };
                    let chunk = tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = tx.closed() => {
                            cancel.cancel();
                            break;
                        },
                        chunk = response.chunk() => chunk,
                    };
                    match chunk {
                        Ok(Some(bytes)) => {
                            let event = StreamEvent {
                                event_type: event_content_type.clone(),
                                binary: Some(bytes.to_vec()),
                                ..StreamEvent::default()
                            };
                            permit.send(Ok(event));
                        }
                        Ok(None) => {
                            // Persistence uses this internal sentinel to mark a
                            // clean binary EOF. The HTTP body adapter ignores
                            // events without a binary payload.
                            let done = StreamEvent {
                                event_type: Some("binary.done".to_string()),
                                ..StreamEvent::default()
                            };
                            permit.send(Ok(done));
                            break;
                        }
                        Err(err) => {
                            let error = ConduitError::upstream(format!(
                                "upstream binary stream read failed: {err}"
                            ));
                            permit.send(Err(error));
                            break;
                        }
                    }
                }
            });

            return Ok(LiveUpstreamResponse {
                content_type,
                events: rx,
            });
        }

        tokio::spawn(async move {
            let mut buffer: Vec<u8> = Vec::new();
            loop {
                if cancel.is_canceled() {
                    // Client disconnected: dropping `response` at end of scope
                    // aborts the upstream HTTP request.
                    break;
                }
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => break,
                    chunk = response.chunk() => chunk,
                };
                match chunk {
                    Ok(Some(bytes)) => {
                        buffer.extend_from_slice(&bytes);
                        // Re-parse the accumulated buffer; `incomplete` is the
                        // partial trailing frame to carry into the next read.
                        match conduit_llm::http::parse_sse_frames(&buffer) {
                            Ok(parsed) => {
                                for frame in parsed.frames {
                                    if tx.send(Ok(StreamEvent::from(frame))).await.is_err() {
                                        // Client receiver dropped mid-stream.
                                        return;
                                    }
                                }
                                buffer = parsed.incomplete;
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(Err(ConduitError::upstream(format!(
                                        "failed to parse upstream stream: {err}"
                                    ))))
                                    .await;
                                return;
                            }
                        }
                    }
                    // Upstream ended cleanly. WHATWG requires an event-ending
                    // blank line; an incomplete tail is intentionally dropped.
                    Ok(None) => break,
                    Err(err) => {
                        let _ = tx
                            .send(Err(ConduitError::upstream(format!(
                                "upstream stream read failed: {err}"
                            ))))
                            .await;
                        return;
                    }
                }
            }
        });

        Ok(LiveUpstreamResponse {
            content_type,
            events: rx,
        })
    }
}

fn is_binary_speech_request(request: &HttpRequest) -> bool {
    request.api_format == Some(conduit_llm::ApiFormat::OpenAiAudioSpeech)
        && request
            .metadata
            .get("audio_stream_mode")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("binary"))
}

async fn validate_sse_response(
    response: reqwest::Response,
) -> Result<reqwest::Response, ConduitError> {
    let mut response = validate_success_response(response).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let content_type = header_string(&headers, reqwest::header::CONTENT_TYPE).unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"))
    {
        let (body, truncated) = read_capped_body(&mut response).await?;
        let provider_body = serde_json::from_slice(&body).unwrap_or_else(|_| {
            serde_json::json!({
                "body": String::from_utf8_lossy(&body),
                "truncated": truncated,
            })
        });
        return Err(ConduitError::upstream(format!(
            "upstream streaming response has unsupported content type {:?}",
            content_type
        ))
        .with_provider_status(status.as_u16())
        .with_provider_body(provider_body)
        .with_provider_headers(header_map_to_llm(&headers)));
    }
    Ok(response)
}

async fn validate_binary_response(
    response: reqwest::Response,
) -> Result<reqwest::Response, ConduitError> {
    let mut response = validate_success_response(response).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let content_type = header_string(&headers, reqwest::header::CONTENT_TYPE).unwrap_or_default();
    // A missing/opaque header falls back to application/octet-stream in the
    // HTTP writer, matching the existing binary response contract. A visible
    // non-binary media type (notably a 200 JSON error envelope) is rejected
    // before any client headers are committed.
    if !content_type.is_empty() && !is_binary_content_type(&content_type) {
        let (body, truncated) = read_capped_body(&mut response).await?;
        let provider_body = serde_json::from_slice(&body).unwrap_or_else(|_| {
            serde_json::json!({
                "body": String::from_utf8_lossy(&body),
                "truncated": truncated,
            })
        });
        return Err(ConduitError::upstream(format!(
            "upstream binary response has unsupported content type {:?}",
            content_type
        ))
        .with_provider_status(status.as_u16())
        .with_provider_body(provider_body)
        .with_provider_headers(header_map_to_llm(&headers)));
    }
    Ok(response)
}

async fn validate_success_response(
    mut response: reqwest::Response,
) -> Result<reqwest::Response, ConduitError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let headers = response.headers().clone();
    let (body, truncated) = read_capped_body(&mut response).await?;
    let provider_body = serde_json::from_slice(&body).unwrap_or_else(|_| {
        serde_json::json!({
            "body": String::from_utf8_lossy(&body),
            "truncated": truncated,
        })
    });
    Err(
        ConduitError::upstream(format!("upstream returned HTTP {status}"))
            .with_provider_status(status.as_u16())
            .with_provider_body(provider_body)
            .with_provider_headers(header_map_to_llm(&headers)),
    )
}

fn is_binary_content_type(content_type: &str) -> bool {
    let media_type = content_type.split(';').next().unwrap_or_default().trim();
    media_type.eq_ignore_ascii_case("application/octet-stream")
        || media_type
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("audio/"))
}

async fn read_capped_body(
    response: &mut reqwest::Response,
) -> Result<(Vec<u8>, bool), ConduitError> {
    let mut body = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await.map_err(|err| {
        ConduitError::upstream(format!("failed to read upstream error body: {err}"))
    })? {
        let remaining = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
        if body.len() == MAX_ERROR_BODY_BYTES {
            truncated = true;
            break;
        }
    }
    Ok((body, truncated))
}

/// Build + send a reqwest request from the transformed [`HttpRequest`].
async fn send(
    client: &reqwest::Client,
    request: &HttpRequest,
) -> Result<reqwest::Response, ConduitError> {
    let url = request.url.as_deref().ok_or_else(|| {
        ConduitError::internal("upstream request has no url (outbound transformer must set it)")
    })?;
    let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|err| {
        ConduitError::internal(format!("invalid http method {:?}: {err}", request.method))
    })?;

    let mut builder = client.request(method, url);

    // Multi-valued query params (BTreeMap<String, Vec<String>> serializes as
    // repeated keys, matching Go's url.Values encoding).
    if !request.query.is_empty() {
        builder = builder.query(&request.query);
    }

    let mut outbound_headers = header_map_from_llm(&request.headers);

    // Prefer a pre-encoded body; fall back to JSON serialization (which also
    // stamps Content-Type: application/json).
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    } else if let Some(json) = &request.json_body {
        builder = builder.json(json);
    }

    // Auth: a scheme + token → `Authorization: <scheme> <token>`. The outbound
    // transformer usually bakes credentials into headers already; this covers
    // the structured `auth` field when present.
    if let Some(auth) = &request.auth
        && let Some(token) = &auth.token
    {
        let scheme = if auth.scheme.trim().is_empty() {
            "Bearer"
        } else {
            auth.scheme.as_str()
        };
        let value = HeaderValue::from_str(&format!("{scheme} {token}")).map_err(|err| {
            ConduitError::internal(format!("invalid outbound authorization header: {err}"))
        })?;
        // The channel's structured auth is authoritative. Replacing the map
        // entry avoids sending two Authorization headers when an inbound
        // request or transformer already supplied one.
        outbound_headers.insert(reqwest::header::AUTHORIZATION, value);
    }

    builder = builder.headers(outbound_headers);

    builder
        .send()
        .await
        .map_err(|err| ConduitError::upstream(format!("upstream request failed: {err}")))
}

/// `conduit_llm::model::HeaderMap` (`BTreeMap<String, String>`) → reqwest header map.
/// Invalid header names/values are skipped (rare for transformer-produced
/// headers; surfacing them as a hard error would let one bad header break a
/// request that Go would otherwise send).
fn header_map_from_llm(llm_headers: &conduit_llm::model::HeaderMap) -> ReqwestHeaderMap {
    let mut map = ReqwestHeaderMap::new();
    for (name, value) in llm_headers {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(value) else {
            continue;
        };
        map.append(name, value);
    }
    map
}

/// reqwest header map → `conduit_llm::model::HeaderMap` for the returned [`HttpResponse`].
fn header_map_to_llm(headers: &ReqwestHeaderMap) -> conduit_llm::model::HeaderMap {
    let mut map = conduit_llm::model::HeaderMap::new();
    for (name, value) in headers.iter() {
        let Some(value_str) = value.to_str().ok() else {
            continue;
        };
        map.insert(name.as_str().to_string(), value_str.to_string());
    }
    map
}

fn header_string(headers: &ReqwestHeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::Sender as StdSender;

    /// One-shot raw HTTP/1.1 server on an ephemeral port: accepts a single
    /// connection, drains the request, writes the canned response bytes, and
    /// exits. Mirrors the `conduit-llm` integration-test pattern (no mock
    /// crate in the workspace).
    fn one_shot_server(response: Vec<u8>) -> Result<String, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/v1/chat/completions");
        std::thread::spawn(move || {
            // `accept` errors are fine if the client never connected (a test
            // failure elsewhere); bail without panicking.
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Drain the request line + headers (and any small body) so the
            // client finishes sending before we write the response.
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        });
        Ok(url)
    }

    fn hanging_sse_server() -> Result<String, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/v1/chat/completions");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
        Ok(url)
    }

    /// Chunked binary server that withholds its second chunk until the test
    /// explicitly releases it. A live executor must return the response and
    /// first chunk while the server is still blocked; an implementation using
    /// `response.bytes()` cannot do so.
    fn split_binary_server(
        first: Vec<u8>,
        second: Vec<u8>,
    ) -> Result<(String, StdSender<()>), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/v1/audio/speech");
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg; codec=mp3\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            let _ = write_http_chunk(&mut stream, &first);
            let _ = stream.flush();
            if release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
            {
                let _ = write_http_chunk(&mut stream, &second);
                let _ = stream.write_all(b"0\r\n\r\n");
                let _ = stream.flush();
            }
        });
        Ok((url, release_tx))
    }

    /// Binary server that leaves the response unfinished after one chunk.
    fn hanging_binary_server(first: Vec<u8>) -> Result<String, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/v1/audio/speech");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/ogg\r\nTransfer-Encoding: chunked\r\n\r\n",
            );
            let _ = write_http_chunk(&mut stream, &first);
            let _ = stream.flush();
            std::thread::sleep(std::time::Duration::from_secs(3));
        });
        Ok(url)
    }

    fn write_http_chunk(stream: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
        write!(stream, "{:X}\r\n", bytes.len())?;
        stream.write_all(bytes)?;
        stream.write_all(b"\r\n")
    }

    fn binary_speech_request(url: String) -> HttpRequest {
        let mut request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            api_format: Some(conduit_llm::ApiFormat::OpenAiAudioSpeech),
            json_body: Some(serde_json::json!({
                "model": "gpt-4o-mini-tts",
                "input": "hello",
                "voice": "alloy",
                "stream_format": "audio"
            })),
            ..HttpRequest::default()
        };
        request.metadata.insert(
            "audio_stream_mode".to_string(),
            serde_json::Value::String("binary".to_string()),
        );
        request
    }

    fn test_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
        // A plain reqwest client is enough to exercise the executor; the
        // production path uses HttpClientBuilder for proxy/timeout parity.
        Ok(reqwest::Client::builder().build()?)
    }

    #[test]
    fn channel_proxy_metadata_accepts_graphql_enum_literals()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut request = HttpRequest::default();
        request.metadata.insert(
            "channel_proxy".to_string(),
            serde_json::Value::String(
                r#"{"type":"URL","url":"http://proxy.example:8080","username":"u","password":"p"}"#
                    .to_string(),
            ),
        );

        let proxy = proxy_config_from_request(&request)?.ok_or("proxy should be present")?;

        assert_eq!(proxy.mode, conduit_llm::ProxyMode::Url);
        assert_eq!(proxy.url.as_deref(), Some("http://proxy.example:8080"));
        assert_eq!(proxy.username.as_deref(), Some("u"));
        assert_eq!(proxy.password.as_deref(), Some("p"));
        Ok(())
    }

    #[test]
    fn scoped_proxy_clients_retain_the_configured_tls_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = UpstreamExecutor::new(test_client()?).with_insecure_skip_verify(true);

        assert!(executor.insecure_skip_verify);
        Ok(())
    }

    #[tokio::test]
    async fn execute_uses_request_scoped_channel_proxy() -> Result<(), Box<dyn std::error::Error>> {
        let body = br#"{"ok":true}"#;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let proxy_endpoint = one_shot_server(response)?;
        let proxy_url = proxy_endpoint
            .strip_suffix("/v1/chat/completions")
            .ok_or("unexpected proxy test URL")?;
        let mut request = HttpRequest {
            method: "POST".to_string(),
            url: Some("http://127.0.0.1:1/provider".to_string()),
            ..HttpRequest::default()
        };
        request.metadata.insert(
            "channel_proxy".to_string(),
            serde_json::Value::String(format!(r#"{{"type":"URL","url":"{proxy_url}"}}"#)),
        );

        let response = UpstreamExecutor::new(test_client()?)
            .execute(&request)
            .await?;

        assert_eq!(response.status, 200);
        assert_eq!(response.json_body, Some(serde_json::json!({"ok": true})));
        Ok(())
    }

    #[tokio::test]
    async fn execute_decodes_json_response() -> Result<(), Box<dyn std::error::Error>> {
        let body = br#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let url = one_shot_server(response)?;

        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            json_body: Some(serde_json::json!({"model": "gpt-4"})),
            ..HttpRequest::default()
        };

        let response = executor.execute(&request).await?;
        assert_eq!(response.status, 200);
        let json = response
            .json_body
            .as_ref()
            .ok_or("json_body should be populated for application/json")?;
        assert_eq!(json["choices"][0]["message"]["content"], "hi");
        Ok(())
    }

    #[tokio::test]
    async fn execute_preserves_binary_audio_response() -> Result<(), Box<dyn std::error::Error>> {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/contracts/llm_cases/openai_audio/speech_stream_binary.json"
        ))?;
        let body: Vec<u8> =
            serde_json::from_value(contract["upstream_http"]["body_bytes"].clone())?;
        let content_type = contract["upstream_http"]["content_type"]
            .as_str()
            .ok_or("missing contract content type")?;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        let url = one_shot_server(response)?;
        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            json_body: Some(contract["inbound_http"]["body_json"].clone()),
            ..HttpRequest::default()
        };

        let response = executor.execute(&request).await?;
        assert_eq!(response.status, 200);
        assert_eq!(response.body.as_deref(), Some(body.as_slice()));
        assert!(response.json_body.is_none());
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some(content_type)
        );
        Ok(())
    }

    #[tokio::test]
    async fn execute_stream_parses_sse_frames() -> Result<(), Box<dyn std::error::Error>> {
        let body = b"data: hello\n\ndata: world\n\ndata: [DONE]\n\n";
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let url = one_shot_server(response)?;

        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            json_body: Some(serde_json::json!({"stream": true})),
            ..HttpRequest::default()
        };

        let events = executor.execute_stream(&request).await?;
        assert_eq!(events.len(), 3, "three SSE frames expected");
        assert_eq!(events[0].data.as_deref(), Some("hello"));
        assert_eq!(events[1].data.as_deref(), Some("world"));
        assert_eq!(events[2].data.as_deref(), Some("[DONE]"));
        Ok(())
    }

    #[tokio::test]
    async fn execute_stream_live_rejects_non_success_before_opening_stream()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = br#"{"error":{"message":"bad key"}}"#;
        let mut response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let url = one_shot_server(response)?;
        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            ..HttpRequest::default()
        };

        let err = executor
            .execute_stream_live(&request, conduit_pipeline::CancelToken::new())
            .await
            .err()
            .ok_or("expected upstream HTTP error")?;
        assert_eq!(err.provider_status, Some(401));
        assert_eq!(
            err.provider_body
                .as_ref()
                .and_then(|v| v.pointer("/error/message")),
            Some(&serde_json::json!("bad key"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn execute_stream_live_rejects_successful_non_sse_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = br#"{"unexpected":true}"#;
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        let url = one_shot_server(response)?;
        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(url),
            ..HttpRequest::default()
        };

        let err = executor
            .execute_stream_live(&request, conduit_pipeline::CancelToken::new())
            .await
            .err()
            .ok_or("expected content-type error")?;
        assert!(err.message.contains("unsupported content type"));
        assert_eq!(err.provider_status, Some(200));
        Ok(())
    }

    #[tokio::test]
    async fn execute_stream_live_cancel_wakes_blocked_body_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let executor = UpstreamExecutor::new(test_client()?);
        let request = HttpRequest {
            method: "POST".to_string(),
            url: Some(hanging_sse_server()?),
            ..HttpRequest::default()
        };
        let cancel = conduit_pipeline::CancelToken::new();
        let mut live = executor
            .execute_stream_live(&request, cancel.clone())
            .await?;
        cancel.cancel();

        let next = tokio::time::timeout(std::time::Duration::from_secs(1), live.events.recv())
            .await
            .map_err(|_| "stream read did not wake after cancellation")?;
        assert!(next.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn binary_speech_live_forwards_chunks_before_upstream_eof_without_sse_parsing()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = vec![0xff, 0x00, b'd', b'a', b't', b'a', b':', b' ', b'[', b'D'];
        let second = vec![b'O', b'N', b'E', b']', b'\n', b'\n', 0x80, 0x01];
        let (url, release) = split_binary_server(first.clone(), second.clone())?;
        let executor = UpstreamExecutor::new(test_client()?);

        // The server is withholding its tail. Returning here proves this path
        // did not call `response.bytes()` or wait for a complete body.
        let mut live = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            executor.execute_stream_live(
                &binary_speech_request(url),
                conduit_pipeline::CancelToken::new(),
            ),
        )
        .await
        .map_err(|_| "binary executor waited for the complete upstream body")??;
        assert_eq!(live.content_type.as_deref(), Some("audio/mpeg; codec=mp3"));

        let first_event =
            tokio::time::timeout(std::time::Duration::from_secs(1), live.events.recv())
                .await?
                .ok_or("missing first binary chunk")??;
        assert_eq!(first_event.binary.as_deref(), Some(first.as_slice()));
        assert!(
            first_event.data.is_none(),
            "binary bytes must not become SSE data"
        );

        release.send(())?;
        let second_event = live.events.recv().await.ok_or("missing second chunk")??;
        assert_eq!(second_event.binary.as_deref(), Some(second.as_slice()));
        let done = live
            .events
            .recv()
            .await
            .ok_or("missing binary EOF sentinel")??;
        assert_eq!(done.event_type.as_deref(), Some("binary.done"));
        assert!(done.binary.is_none());
        assert!(live.events.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn binary_speech_live_accepts_missing_content_type_for_http_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let body = vec![0xff, 0x00, 0x80, b'd', b'a', b't', b'a', b':'];
        let mut response =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        response.extend_from_slice(&body);
        let executor = UpstreamExecutor::new(test_client()?);
        let mut live = executor
            .execute_stream_live(
                &binary_speech_request(one_shot_server(response)?),
                conduit_pipeline::CancelToken::new(),
            )
            .await?;

        assert!(
            live.content_type.is_none(),
            "the HTTP layer must own the application/octet-stream fallback"
        );
        let chunk = live.events.recv().await.ok_or("missing binary chunk")??;
        assert_eq!(chunk.binary.as_deref(), Some(body.as_slice()));
        let done = live
            .events
            .recv()
            .await
            .ok_or("missing binary EOF sentinel")??;
        assert_eq!(done.event_type.as_deref(), Some("binary.done"));
        assert!(live.events.recv().await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dropping_binary_speech_receiver_cancels_unfinished_upstream_response()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = vec![0x4f, 0x67, 0x67, 0x53];
        let url = hanging_binary_server(first.clone())?;
        let executor = UpstreamExecutor::new(test_client()?);
        let cancel = conduit_pipeline::CancelToken::new();
        let mut live = executor
            .execute_stream_live(&binary_speech_request(url), cancel.clone())
            .await?;
        let event = live
            .events
            .recv()
            .await
            .ok_or("missing first audio chunk")??;
        assert_eq!(event.binary.as_deref(), Some(first.as_slice()));

        // This models the downstream Axum body being dropped on client
        // disconnect. The sender's next reserve fails, ending the read task and
        // dropping reqwest::Response without draining the provider.
        drop(live.events);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !cancel.is_canceled() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| "upstream task did not observe the downstream receiver drop")?;
        Ok(())
    }

    #[tokio::test]
    async fn execute_surfaces_missing_url_as_internal_error() {
        let executor = UpstreamExecutor::new(
            test_client()
                .unwrap_or_else(|err| panic!("failed to build test reqwest client: {err}")),
        );
        let request = HttpRequest {
            method: "POST".to_string(),
            // url deliberately omitted — the outbound transformer must set it.
            ..HttpRequest::default()
        };

        let err = match executor.execute(&request).await {
            Ok(_) => panic!("expected an error when url is missing"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("no url"),
            "error must explain the missing url: got {err}"
        );
    }
}

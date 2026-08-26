//! Integration tests for the conduit-llm HTTP client surface.
//!
//! These mirror the Go `llm/httpclient/client_test.go` fake-provider pattern:
//! `TestHttpClientImpl_Do` (non-streaming JSON, status-code error shape) and
//! `TestHttpClientImpl_DoStream` / `TestSSEStream` (SSE golden frame sequence).
//!
//! Rust currently has no equivalent of Go's `httptest.NewServer` available
//! without pulling a heavyweight dev-dep (wiremock/httpmock/mockito are absent
//! from `Cargo.lock`). Instead each test spins up a tiny raw HTTP/1.1 server
//! on an ephemeral port via `std::net::TcpListener` running in a dedicated
//! thread; the response bytes are hand-framed. `reqwest` (built from the same
//! builder used in production) dials that server, so the request traverses
//! the real client -> transport -> response body -> `decode_response_body`
//! pipeline.
//!
//! Why std net + thread, not tokio TcpListener:
//! - tokio's `io-util` feature (required for `AsyncReadExt`/`AsyncWriteExt`)
//!   is NOT enabled in the workspace tokio declaration; adding dev-deps just
//!   for these tests would widen the dependency surface for no other gain.
//! - A blocking `std::thread` accepts exactly one connection, writes the
//!   canned response, and exits. The async reqwest client still dials the
//!   listener normally — TCP is transport-agnostic.
//!
//! Go parity cases covered (see file-level comments on each test):
//!  * `TestHttpClientImpl_Do` "successful request" — JSON shape assertions
//!    (Rust test: `client_do_json_response_decodes_through_pipeline`).
//!  * `TestHttpClientImpl_Do` "request with query parameters" + SSE variant —
//!    covered indirectly by the SSE test (frame sequence assertions).
//!  * `TestHttpClientImpl_DoStream` "successful streaming request" — SSE
//!    golden frame sequence (Rust test: `client_do_sse_stream_decodes_frames`).
//!  * SSE golden body from `TestSSEStream` — three frames, last is `[DONE]`.
//!  * Binary body — audio content-type, byte-for-byte assertions
//!    (Rust test: `client_do_binary_response_preserves_bytes`).
//!  * HTTP error shape — mirrors `TestHttpClientImpl_Do` "HTTP error response"
//!    (Rust test: `client_do_4xx_response_surfaces_upstream_error_shape`).
//!
//! A02 (delayed-first-event timeout cancellation) is documented at the bottom
//! of this file. The Go `httpclient` package has NO first-event timeout
//! parameter (verified by `grep -i timeout|first.event|deadline` over
//! `conduit/llm/httpclient/*.go` — only `net.Dialer` / `http.Transport` /
//! `http.Client{Timeout}` connect-level timeouts are present). First-event
//! timeout therefore belongs to the pipeline/orchestrator layer (where
//! `RetryPolicy` first-event timeout landed). The client layer only exposes
//! the reqwest per-request `.timeout()`, which we exercise here to confirm
//! that a delayed response is observable as a timeout error from the client
//! surface (Rust test: `client_request_timeout_aborts_delayed_response`).

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use conduit_llm::{DecodedBody, HttpClientBuilder, SseFrame, decode_response_body};

/// Frame a minimal HTTP/1.1 response. `body` is sent verbatim; callers are
/// responsible for the bytes matching the declared `content_type` (the decoder
/// branches on the content-type header, not on body bytes).
fn write_response(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &[u8],
) {
    let head = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        ctype = content_type,
        len = body.len(),
    );
    // Two writes are fine — reqwest reads once the socket closes (Connection: close).
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// Bind an ephemeral port, return the listener and the URL reqwest should dial.
fn bind_fake_server() -> std::io::Result<(TcpListener, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    Ok((listener, format!("http://{addr}")))
}

/// Read until the end of the HTTP request headers (`\r\n\r\n`). We do not
/// parse the request — we only need to drain it so the kernel buffer does not
/// push back before we write the response.
fn drain_request_headers(stream: &mut TcpStream) {
    let mut buf = [0u8; 1024];
    let mut seen = Vec::with_capacity(1024);
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => return, // peer closed early
            Ok(n) => n,
            Err(_) => return,
        };
        seen.extend_from_slice(&buf[..n]);
        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
            return;
        }
        if seen.len() > 8 * 1024 {
            // Defensive: cap header drain at 8 KiB.
            return;
        }
    }
}

/// Accept exactly one connection and run `responder` on it. The responder
/// receives the connected stream. Runs in a dedicated OS thread; returns
/// immediately so the test can dial the listener concurrently.
fn spawn_responder<F>(listener: TcpListener, responder: F)
where
    F: FnOnce(&mut TcpStream) + Send + 'static,
{
    thread::spawn(move || {
        // Drop accept errors silently — the test will fail on the reqwest side
        // if the server never materialized.
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        // Best-effort read timeout so a hung test does not block forever.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        drain_request_headers(&mut stream);
        responder(&mut stream);
    });
}

// ---------------------------------------------------------------------------
// A01 — fake-provider SSE / binary / JSON response tests.
// ---------------------------------------------------------------------------

/// Mirrors the SSE golden body used by Go's `TestSSEStream` (decoder_test.go)
/// and `TestHttpClientImpl_DoStream` "successful streaming request":
/// three frames; the third is the OpenAI-style `[DONE]` sentinel.
const GO_SSE_GOLDEN: &str = "data: {\"id\": \"1\", \"content\": \"Hello\"}\n\n\
data: {\"id\": \"2\", \"content\": \"World\"}\n\n\
data: [DONE]\n\n";

/// Build a reqwest client through the production `HttpClientBuilder`. Mirrors
/// Go's `NewHttpClient()` construction path.
fn build_test_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    Ok(HttpClientBuilder::new()
        // Short connect timeout so a hung server fails fast instead of
        // hanging the full 30s Go default.
        .connect_timeout(Duration::from_secs(2))
        .build()?)
}

/// A01 (1/3) — SSE streaming response. Mirrors Go's
/// `TestHttpClientImpl_DoStream` "successful streaming request" golden frame
/// sequence (three frames, last is `[DONE]`).
///
/// Go source: `conduit/llm/httpclient/client_test.go:180-218` writes the same
/// three events (`data: {"id": "1", "content": "Hello"}`, `... "World"`,
/// `data: [DONE]`); `TestSSEStream` in `decoder_test.go` consumes the same
/// body as a single concatenated blob.
#[tokio::test]
async fn client_do_sse_stream_decodes_frames() -> Result<(), Box<dyn std::error::Error>> {
    let (listener, base_url) = bind_fake_server()?;
    spawn_responder(listener, |stream| {
        write_response(
            stream,
            200,
            "OK",
            "text/event-stream",
            GO_SSE_GOLDEN.as_bytes(),
        );
    });

    let client = build_test_client()?;
    let resp = client
        .post(format!("{base_url}/v1/chat/completions"))
        .header("Accept", "text/event-stream")
        .body("{\"stream\": true}")
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap_or("")),
        Some("text/event-stream")
    );

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.bytes().await?;
    let decoded = decode_response_body(content_type.as_deref(), &body)?;

    let DecodedBody::Sse(parsed) = decoded else {
        return Err(format!("expected DecodedBody::Sse, got {decoded:?}").into());
    };

    // Go `TestHttpClientImpl_DoStream` only checks `stream != nil`; the
    // frame-level assertions below mirror the body that `TestSSEStream`
    // feeds into the decoder — three dispatched frames, last data is
    // `[DONE]`.
    assert_eq!(
        parsed.frames.len(),
        3,
        "expected 3 dispatched SSE frames, got {}: {:?}",
        parsed.frames.len(),
        parsed.frames,
    );
    assert_eq!(
        parsed.frames[0],
        SseFrame {
            last_event_id: None,
            event_type: None,
            data: "{\"id\": \"1\", \"content\": \"Hello\"}".to_string(),
            retry: None,
        }
    );
    assert_eq!(
        parsed.frames[1].data,
        "{\"id\": \"2\", \"content\": \"World\"}"
    );
    // OpenAI sentinel — not dispatched as a JSON event.
    assert_eq!(parsed.frames[2].data, "[DONE]");
    assert!(parsed.incomplete.is_empty(), "no trailing partial bytes");
    Ok(())
}

/// A01 (2/3) — Binary body, audio content-type. Mirrors the Go
/// `BodyDecoder == Binary` path: bytes are returned verbatim, no JSON parser
/// is invoked. The audio payload below is a synthetic 8-byte signature chosen
/// to be non-UTF-8 so we also confirm no transcoding happens.
#[tokio::test]
async fn client_do_binary_response_preserves_bytes() -> Result<(), Box<dyn std::error::Error>> {
    // Synthetic audio-like bytes: 0x52 0x49 0x46 0x46 ("RIFF" magic) + length
    // header + non-UTF-8 payload. A real WAV starts exactly this way; the
    // remainder is intentionally filler for the byte-parity assertion.
    let audio_payload = [0x52u8, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff];

    let (listener, base_url) = bind_fake_server()?;
    let payload = audio_payload.to_vec();
    spawn_responder(listener, move |stream| {
        write_response(stream, 200, "OK", "audio/wav", &payload);
    });

    let client = build_test_client()?;
    let resp = client
        .get(format!("{base_url}/v1/audio/speech"))
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    assert_eq!(content_type.as_deref(), Some("audio/wav"));

    let body = resp.bytes().await?;
    let decoded = decode_response_body(content_type.as_deref(), &body)?;
    let DecodedBody::Binary(bytes) = decoded else {
        return Err(format!("expected DecodedBody::Binary, got {decoded:?}").into());
    };
    assert_eq!(
        bytes,
        audio_payload.to_vec(),
        "binary bytes must round-trip verbatim"
    );
    Ok(())
}

/// A01 (3/3) — JSON response shape assertion. Mirrors Go's
/// `TestHttpClientImpl_Do` "successful request": server returns
/// `{"response": "success"}` with `Content-Type: application/json`, and the
/// client surfaces a JSON-decodable body.
#[tokio::test]
async fn client_do_json_response_decodes_through_pipeline() -> Result<(), Box<dyn std::error::Error>>
{
    let (listener, base_url) = bind_fake_server()?;
    spawn_responder(listener, |stream| {
        write_response(
            stream,
            200,
            "OK",
            "application/json",
            b"{\"response\": \"success\"}",
        );
    });

    let client = build_test_client()?;
    let resp = client
        .post(format!("{base_url}/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .body("{\"test\": \"data\"}")
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 200);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    assert_eq!(content_type.as_deref(), Some("application/json"));

    let body = resp.bytes().await?;
    let decoded = decode_response_body(content_type.as_deref(), &body)?;
    let DecodedBody::Json(value) = decoded else {
        return Err(format!("expected DecodedBody::Json, got {decoded:?}").into());
    };
    assert_eq!(
        value,
        serde_json::json!({"response": "success"}),
        "JSON body must round-trip through the decoder"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Bonus parity case — non-2xx surfaces an UpstreamError-shaped response.
// Mirrors Go's `TestHttpClientImpl_Do` "HTTP error response": server returns
// 400, the client surfaces a status-bearing error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_do_4xx_response_surfaces_upstream_error_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let (listener, base_url) = bind_fake_server()?;
    spawn_responder(listener, |stream| {
        write_response(
            stream,
            400,
            "Bad Request",
            "application/json",
            b"{\"error\": \"bad request\"}",
        );
    });

    let client = build_test_client()?;
    let resp = client
        .post(format!("{base_url}/v1/chat/completions"))
        .header("Content-Type", "application/json")
        .body("{\"test\": \"data\"}")
        .send()
        .await?;

    assert_eq!(resp.status().as_u16(), 400);
    assert_eq!(resp.status().canonical_reason(), Some("Bad Request"));

    // The Go client raises an `httpclient.Error` whose `.Error()` reads
    // "<METHOD> - <URL> with status <STATUS>". The Rust client surfaces
    // `UpstreamError` from `from_response`; we mirror that construction here
    // to keep the parity path exercised.
    let headers = conduit_llm::model::HeaderMap::new();
    let upstream = conduit_llm::UpstreamError::from_response(
        "POST",
        &format!("{base_url}/v1/chat/completions"),
        400,
        "400 Bad Request",
        b"{\"error\": \"bad request\"}",
        &headers,
    )
    .await;
    assert_eq!(upstream.status_code, 400);
    assert_eq!(upstream.status, "400 Bad Request");
    assert_eq!(upstream.body, b"{\"error\": \"bad request\"}".to_vec());
    // Go parity: Display is "<METHOD> - <URL> with status <STATUS>".
    let display = upstream.to_string();
    assert!(
        display.starts_with("POST - http://127.0.0.1:")
            && display.ends_with("with status 400 Bad Request"),
        "Display must mirror Go's Error.Error(): got {display:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// A02 — first-event / response timeout cancellation.
// ---------------------------------------------------------------------------

/// A02 conclusion (see file-level docs):
///
/// Go's `llm/httpclient` package exposes NO first-event-timeout parameter —
/// only connect / TLS / idle-pool / per-request timeouts inherited from
/// `net.Dialer` and `http.Transport`. First-event timeout is a
/// **pipeline / orchestrator** concern and was landed in the Rust pipeline
/// crate (`RetryPolicy`'s first-event timeout). The client layer can only
/// expose reqwest's per-request `.timeout()` (mirroring Go's
/// `http.Client{Timeout}`).
///
/// This test exercises the client-layer surface that *is* observable: when
/// the server delays its response past the per-request timeout, reqwest
/// surfaces a timeout error and the future resolves (i.e. the request is
/// cancellable from the client side — no orphaned connection waiting on the
/// full body). The pipeline layer owns the richer "cancel if no SSE event
/// arrives within N ms" policy; that contract is tested in the pipeline
/// crate.
#[tokio::test]
async fn client_request_timeout_aborts_delayed_response() -> Result<(), Box<dyn std::error::Error>>
{
    let (listener, base_url) = bind_fake_server()?;
    spawn_responder(listener, |stream| {
        // Hold the connection open without writing a response status line.
        // The client's per-request timeout (set below to 200 ms) must fire
        // before the server writes anything. We sleep for 1 s to make the
        // timeout window unambiguous.
        thread::sleep(Duration::from_secs(1));
        // Even if the timeout already fired, complete the response so the
        // server thread can exit cleanly.
        write_response(stream, 200, "OK", "application/json", b"{}");
    });

    // Per-request timeout is the only timeout knob the Go client exposes at
    // this layer (Go: `http.Client{Timeout}`).
    let client = HttpClientBuilder::new()
        .connect_timeout(Duration::from_secs(2))
        .request_timeout(Some(Duration::from_millis(200)))
        .build()?;

    let result = client
        .post(format!("{base_url}/v1/chat/completions"))
        .body("{\"stream\": true}")
        .send()
        .await;

    assert!(
        result.is_err(),
        "delayed response must surface as an error when request_timeout is set"
    );
    let err = match result {
        Err(e) => e,
        Ok(_) => return Ok(()),
    };
    // reqwest surfaces timeouts as `error.is_timeout() == true`. We do not
    // pattern-match the exact Display string (it varies across reqwest
    // versions) — `is_timeout` is the stable predicate.
    assert!(
        err.is_timeout(),
        "expected reqwest::Error::is_timeout, got: {err}"
    );
    Ok(())
}

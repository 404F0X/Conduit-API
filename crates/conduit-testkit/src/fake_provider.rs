use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::routing::any;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::fixtures::{BodyFixture, HeaderFixture, HttpFixture};
use crate::stream::SseEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedRequest {
    pub request: HttpFixture,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeRoute {
    method: String,
    path: String,
    response: FakeResponse,
}

impl FakeRoute {
    pub fn new(method: impl Into<String>, path: impl Into<String>, response: FakeResponse) -> Self {
        Self {
            method: method.into().to_ascii_uppercase(),
            path: path.into(),
            response,
        }
    }

    fn matches(&self, request: &HttpFixture) -> bool {
        self.method.eq_ignore_ascii_case(&request.method) && self.path == request.path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FakeResponse {
    Json {
        status: u16,
        headers: Vec<HeaderFixture>,
        body: String,
    },
    Sse {
        status: u16,
        headers: Vec<HeaderFixture>,
        events: Vec<SseEvent>,
    },
    Binary {
        status: u16,
        headers: Vec<HeaderFixture>,
        body: Vec<u8>,
    },
    Body {
        status: u16,
        headers: Vec<HeaderFixture>,
        body: BodyFixture,
    },
    EmptyStream {
        status: u16,
        headers: Vec<HeaderFixture>,
    },
}

impl FakeResponse {
    pub fn status(&self) -> u16 {
        match self {
            Self::Json { status, .. }
            | Self::Sse { status, .. }
            | Self::Binary { status, .. }
            | Self::Body { status, .. }
            | Self::EmptyStream { status, .. } => *status,
        }
    }

    pub fn headers(&self) -> &[HeaderFixture] {
        match self {
            Self::Json { headers, .. }
            | Self::Sse { headers, .. }
            | Self::Binary { headers, .. }
            | Self::Body { headers, .. }
            | Self::EmptyStream { headers, .. } => headers,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FakeProvider {
    routes: Vec<FakeRoute>,
    responses: VecDeque<FakeResponse>,
    delay_first_byte: Option<Duration>,
    record_requests: bool,
    recorded_requests: Vec<RecordedRequest>,
}

impl FakeProvider {
    pub fn builder() -> FakeProviderBuilder {
        FakeProviderBuilder::default()
    }

    pub fn delay_first_byte(&self) -> Option<Duration> {
        self.delay_first_byte
    }

    pub fn recorded_requests(&self) -> &[RecordedRequest] {
        &self.recorded_requests
    }

    pub fn handle(&mut self, request: HttpFixture) -> Option<FakeResponse> {
        if self.record_requests {
            self.recorded_requests.push(RecordedRequest {
                request: request.clone(),
            });
        }

        if let Some(route) = self.routes.iter().find(|route| route.matches(&request)) {
            return Some(route.response.clone());
        }

        self.responses.pop_front()
    }

    pub fn is_exhausted(&self) -> bool {
        self.responses.is_empty() && self.routes.is_empty()
    }

    pub async fn start_http(self) -> Result<FakeProviderServer, std::io::Error> {
        let provider = Arc::new(Mutex::new(self));
        let state = HttpProviderState {
            provider: Arc::clone(&provider),
        };
        let app = Router::new()
            .fallback(any(handle_http_request))
            .with_state(state);

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join_handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        Ok(FakeProviderServer {
            base_url: format!("http://{addr}"),
            addr,
            provider,
            shutdown_tx: Some(shutdown_tx),
            join_handle,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct FakeProviderBuilder {
    routes: Vec<FakeRoute>,
    responses: VecDeque<FakeResponse>,
    delay_first_byte: Option<Duration>,
    record_requests: bool,
}

impl FakeProviderBuilder {
    pub fn route_response(
        mut self,
        method: impl Into<String>,
        path: impl Into<String>,
        response: FakeResponse,
    ) -> Self {
        self.routes.push(FakeRoute::new(method, path, response));
        self
    }

    pub fn json_once(mut self, body: impl Into<String>) -> Self {
        self.responses.push_back(FakeResponse::Json {
            status: 200,
            headers: vec![HeaderFixture::new("content-type", "application/json")],
            body: body.into(),
        });
        self
    }

    pub fn sse_sequence(mut self, events: impl IntoIterator<Item = SseEvent>) -> Self {
        self.responses.push_back(FakeResponse::Sse {
            status: 200,
            headers: vec![HeaderFixture::new("content-type", "text/event-stream")],
            events: events.into_iter().collect(),
        });
        self
    }

    pub fn binary_once(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.responses.push_back(FakeResponse::Binary {
            status: 200,
            headers: vec![HeaderFixture::new(
                "content-type",
                "application/octet-stream",
            )],
            body: body.into(),
        });
        self
    }

    pub fn empty_stream(mut self) -> Self {
        self.responses.push_back(FakeResponse::EmptyStream {
            status: 200,
            headers: vec![HeaderFixture::new("content-type", "text/event-stream")],
        });
        self
    }

    pub fn delay_first_byte(mut self, delay: Duration) -> Self {
        self.delay_first_byte = Some(delay);
        self
    }

    pub fn status_with_body(mut self, status: u16, body: BodyFixture) -> Self {
        self.responses.push_back(FakeResponse::Body {
            status,
            headers: Vec::new(),
            body,
        });
        self
    }

    pub fn record_requests(mut self) -> Self {
        self.record_requests = true;
        self
    }

    pub fn build(self) -> FakeProvider {
        FakeProvider {
            routes: self.routes,
            responses: self.responses,
            delay_first_byte: self.delay_first_byte,
            record_requests: self.record_requests,
            recorded_requests: Vec::new(),
        }
    }

    pub async fn start_http(self) -> Result<FakeProviderServer, std::io::Error> {
        self.build().start_http().await
    }
}

#[derive(Clone)]
struct HttpProviderState {
    provider: Arc<Mutex<FakeProvider>>,
}

pub struct FakeProviderServer {
    base_url: String,
    addr: SocketAddr,
    provider: Arc<Mutex<FakeProvider>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: JoinHandle<Result<(), std::io::Error>>,
}

impl FakeProviderServer {
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.provider.lock().await.recorded_requests().to_vec()
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
        }

        match self.join_handle.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(format!("fake provider server error: {error}")),
            Err(error) => Err(format!("fake provider server join error: {error}")),
        }
    }
}

async fn handle_http_request(
    State(state): State<HttpProviderState>,
    request: Request<Body>,
) -> Response {
    let delay = state.provider.lock().await.delay_first_byte();
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }

    let fixture = request_to_fixture(request).await;
    let response = state.provider.lock().await.handle(fixture);

    match response {
        Some(response) => response_to_http(response),
        None => plain_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            Body::from("fake provider response queue exhausted"),
            &[],
        ),
    }
}

async fn request_to_fixture(request: Request<Body>) -> HttpFixture {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 32 * 1024 * 1024).await.unwrap_or_default();
    let body = match String::from_utf8(bytes.to_vec()) {
        Ok(text) => BodyFixture::Text(text),
        Err(_) => BodyFixture::Binary(bytes.to_vec()),
    };
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            HeaderFixture::new(
                name.as_str(),
                value.to_str().unwrap_or("<non-utf8-header-value>"),
            )
        })
        .collect();

    HttpFixture {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map(|path| path.as_str().to_string())
            .unwrap_or_else(|| parts.uri.path().to_string()),
        headers,
        body,
    }
}

fn response_to_http(response: FakeResponse) -> Response {
    match response {
        FakeResponse::Json {
            status,
            headers,
            body,
        } => response_with_headers(status, Body::from(body), &headers),
        FakeResponse::Sse {
            status,
            headers,
            events,
        } => response_with_headers(status, Body::from(encode_sse(&events)), &headers),
        FakeResponse::Binary {
            status,
            headers,
            body,
        } => response_with_headers(status, Body::from(body), &headers),
        FakeResponse::Body {
            status,
            headers,
            body,
        } => response_with_headers(status, body_fixture_to_http(body), &headers),
        FakeResponse::EmptyStream { status, headers } => {
            response_with_headers(status, Body::empty(), &headers)
        }
    }
}

fn body_fixture_to_http(body: BodyFixture) -> Body {
    match body {
        BodyFixture::Empty => Body::empty(),
        BodyFixture::Json(body) | BodyFixture::Text(body) => Body::from(body),
        BodyFixture::Binary(body) => Body::from(body),
    }
}

fn response_with_headers(status: u16, body: Body, headers: &[HeaderFixture]) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    plain_response(status, body, headers)
}

fn plain_response(status: StatusCode, body: Body, headers: &[HeaderFixture]) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;

    for header in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(header.name.as_bytes()),
            HeaderValue::from_str(&header.value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }

    response
}

fn encode_sse(events: &[SseEvent]) -> String {
    let mut encoded = String::new();

    for event in events {
        for comment in &event.comments {
            encoded.push(':');
            encoded.push(' ');
            encoded.push_str(comment);
            encoded.push('\n');
        }
        if let Some(id) = &event.id {
            encoded.push_str("id: ");
            encoded.push_str(id);
            encoded.push('\n');
        }
        if let Some(name) = &event.event {
            encoded.push_str("event: ");
            encoded.push_str(name);
            encoded.push('\n');
        }
        if let Some(retry) = event.retry {
            encoded.push_str("retry: ");
            encoded.push_str(&retry.to_string());
            encoded.push('\n');
        }
        for line in event.data.lines() {
            encoded.push_str("data: ");
            encoded.push_str(line);
            encoded.push('\n');
        }
        if event.data.is_empty() {
            encoded.push_str("data:\n");
        }
        encoded.push('\n');
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::time::Instant;

    #[test]
    fn records_requests_and_returns_scripted_response() -> Result<(), Box<dyn std::error::Error>> {
        let mut provider = FakeProvider::builder()
            .record_requests()
            .json_once(r#"{"ok":true}"#)
            .build();

        let response = provider
            .handle(HttpFixture::new("POST", "/v1/chat/completions"))
            .ok_or("scripted response")?;

        assert_eq!(response.status(), 200);
        assert_eq!(provider.recorded_requests().len(), 1);
        assert!(provider.is_exhausted());
        Ok(())
    }

    #[tokio::test]
    async fn http_server_returns_json_and_records_request() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = FakeProvider::builder()
            .record_requests()
            .json_once(r#"{"ok":true}"#)
            .start_http()
            .await?;

        let mut stream = TcpStream::connect(server.addr()).await?;
        stream
            .write_all(
                b"POST /v1/chat/completions HTTP/1.1\r\n\
                  Host: localhost\r\n\
                  Content-Type: application/json\r\n\
                  Connection: close\r\n\
                  Content-Length: 13\r\n\
                  \r\n\
                  {\"ping\":true}",
            )
            .await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        let response = String::from_utf8(response)?;

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains(r#"{"ok":true}"#));

        let recorded = server.recorded_requests().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].request.method, "POST");
        assert_eq!(recorded[0].request.path, "/v1/chat/completions");

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn route_match_returns_429_and_5xx_error_bodies() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = FakeProvider::builder()
            .route_response(
                "POST",
                "/v1/chat/completions",
                FakeResponse::Body {
                    status: 429,
                    headers: vec![HeaderFixture::new("content-type", "application/json")],
                    body: BodyFixture::Json(r#"{"error":{"type":"rate_limit"}}"#.to_string()),
                },
            )
            .route_response(
                "GET",
                "/upstream-error",
                FakeResponse::Body {
                    status: 503,
                    headers: vec![HeaderFixture::new("content-type", "application/json")],
                    body: BodyFixture::Json(r#"{"error":"provider unavailable"}"#.to_string()),
                },
            )
            .start_http()
            .await?;

        let rate_limited = send_raw(
            server.addr(),
            b"POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )
        .await?;
        assert!(rate_limited.starts_with(b"HTTP/1.1 429 Too Many Requests"));
        assert!(contains_bytes(
            &rate_limited,
            br#"{"error":{"type":"rate_limit"}}"#
        ));

        let upstream_error = send_raw(
            server.addr(),
            b"GET /upstream-error HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        assert!(upstream_error.starts_with(b"HTTP/1.1 503 Service Unavailable"));
        assert!(contains_bytes(
            &upstream_error,
            br#"{"error":"provider unavailable"}"#
        ));

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn route_match_returns_sse_and_binary_responses() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = FakeProvider::builder()
            .route_response(
                "GET",
                "/stream",
                FakeResponse::Sse {
                    status: 200,
                    headers: vec![HeaderFixture::new("content-type", "text/event-stream")],
                    events: vec![
                        SseEvent::named("message", "hello"),
                        SseEvent::data("[DONE]"),
                    ],
                },
            )
            .route_response(
                "GET",
                "/audio",
                FakeResponse::Binary {
                    status: 200,
                    headers: vec![HeaderFixture::new(
                        "content-type",
                        "application/octet-stream",
                    )],
                    body: vec![0, 1, 2, 255],
                },
            )
            .start_http()
            .await?;

        let stream = send_raw(
            server.addr(),
            b"GET /stream HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        assert!(contains_bytes(&stream, b"event: message\ndata: hello\n\n"));
        assert!(contains_bytes(&stream, b"data: [DONE]\n\n"));

        let audio = send_raw(
            server.addr(),
            b"GET /audio HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        assert!(audio.starts_with(b"HTTP/1.1 200 OK"));
        assert!(audio.ends_with(&[0, 1, 2, 255]));

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn records_method_path_and_falls_back_to_queue() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = FakeProvider::builder()
            .record_requests()
            .route_response(
                "GET",
                "/matched",
                FakeResponse::Json {
                    status: 200,
                    headers: vec![HeaderFixture::new("content-type", "application/json")],
                    body: r#"{"matched":true}"#.to_string(),
                },
            )
            .json_once(r#"{"queued":true}"#)
            .start_http()
            .await?;

        let matched = send_raw(
            server.addr(),
            b"GET /matched HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;
        assert!(contains_bytes(&matched, br#"{"matched":true}"#));

        let queued = send_raw(
            server.addr(),
            b"POST /fallback HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        )
        .await?;
        assert!(contains_bytes(&queued, br#"{"queued":true}"#));

        let recorded = server.recorded_requests().await;
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].request.method, "GET");
        assert_eq!(recorded[0].request.path, "/matched");
        assert_eq!(recorded[1].request.method, "POST");
        assert_eq!(recorded[1].request.path, "/fallback");

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn delay_first_byte_delays_http_response() -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeProvider::builder()
            .delay_first_byte(Duration::from_millis(25))
            .json_once(r#"{"delayed":true}"#)
            .start_http()
            .await?;

        let started = Instant::now();
        let response = send_raw(
            server.addr(),
            b"GET /delayed HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;

        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(contains_bytes(&response, br#"{"delayed":true}"#));

        server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn empty_stream_returns_sse_headers_without_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = FakeProvider::builder().empty_stream().start_http().await?;

        let response = send_raw(
            server.addr(),
            b"GET /empty-stream HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await?;

        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert!(contains_bytes(
            &response,
            b"content-type: text/event-stream"
        ));
        assert!(response.ends_with(b"\r\n\r\n"));

        server.shutdown().await?;
        Ok(())
    }

    async fn send_raw(addr: SocketAddr, request: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(request).await?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok(response)
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }
}

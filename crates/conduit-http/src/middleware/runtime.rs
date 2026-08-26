use std::net::IpAddr;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;
use crate::middleware::{
    TracingHeaderConfig, extract_client_ip_candidates, insert_request_context, is_blocked_ip,
    operation_name_for_logging, request_context_for_route, resolve_logging_trace_id,
};
use crate::router::{RouteGroupKind, route_group_for_path, strip_base_path_for_mount};

/// Production request middleware shared by every route.
///
/// This is deliberately one outer layer: CORS preflights must bypass route
/// authentication, and the timeout/access-log code must observe the complete
/// downstream request rather than an individual route group.
pub async fn production_request_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    let route_path = strip_base_path_for_mount(&path, state.base_path()).unwrap_or(&path);
    let group = route_group_for_path(route_path);

    if should_apply_ip_blocklist(route_path, group.kind)
        && request_ip_is_blocked(&state, request.headers()).await
    {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({
                "error": {
                    "type": "Forbidden",
                    "message": "IP address is blocked"
                }
            })),
        )
            .into_response();
    }

    if let Some(response) = validate_cors(&state, &method, request.headers(), origin.as_deref()) {
        return response;
    }

    let client_ip = client_ip(request.headers());
    let context = request_context_for_route(
        matches!(group.kind, crate::router::RouteGroupKind::Playground),
        None,
        client_ip,
        request.headers().clone(),
    );
    insert_request_context(&mut request, context);

    let trace_config = tracing_config(&state);
    let trace_id = resolve_logging_trace_id(request.headers(), &trace_config);
    let request_id = new_request_id();
    let operation = operation_name_for_logging(&method, &path);
    let timeout = group.timeout_duration(&state);
    let started = Instant::now();

    let mut response = match tokio::time::timeout(timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            axum::Json(json!({
                "error": {
                    "type": "timeout",
                    "message": "request deadline exceeded"
                }
            })),
        )
            .into_response(),
    };

    insert_header(
        response.headers_mut(),
        trace_config.effective_trace_header(),
        &trace_id,
    );
    insert_header(
        response.headers_mut(),
        trace_config.effective_request_header(),
        &request_id,
    );
    apply_cors_headers(&state, response.headers_mut(), origin.as_deref());

    if response.status().is_client_error() || response.status().is_server_error() {
        tracing::warn!(
            status = response.status().as_u16(),
            method = %method,
            path = %path,
            client_ip = client_ip.map(|ip| ip.to_string()).as_deref().unwrap_or(""),
            operation = operation.as_deref().unwrap_or(""),
            trace_id = %trace_id,
            request_id = %request_id,
            latency_ms = started.elapsed().as_millis() as u64,
            "http request failed"
        );
    }

    response
}

fn should_apply_ip_blocklist(path: &str, kind: RouteGroupKind) -> bool {
    kind == RouteGroupKind::LlmApi || path == "/openapi" || path.starts_with("/openapi/")
}

async fn request_ip_is_blocked(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(system) = state.services().system_service() else {
        return false;
    };
    let blocked_ips = match system.blocked_ips().await {
        Ok(blocked_ips) => blocked_ips,
        Err(error) => {
            tracing::warn!(%error, "failed to load IP blocklist");
            return false;
        }
    };
    if blocked_ips.is_empty() {
        return false;
    }

    let forwarded_headers = ["x-forwarded-for", "x-real-ip"]
        .into_iter()
        .filter_map(|name| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (name.to_string(), value.to_string()))
        })
        .collect::<Vec<_>>();
    let candidates = extract_client_ip_candidates(None, &forwarded_headers);
    is_blocked_ip(&candidates, &blocked_ips)
}

fn validate_cors(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    origin: Option<&str>,
) -> Option<Response> {
    let Some(origin) = origin else {
        return None;
    };
    let config = &state.config().server.cors;
    let allowed_origin = config.allowed_origins.is_empty()
        || config
            .allowed_origins
            .iter()
            .any(|allowed| allowed == "*" || allowed == origin);
    if !allowed_origin {
        return Some((StatusCode::FORBIDDEN, "CORS origin is not allowed").into_response());
    }

    let requested = headers
        .get(header::ACCESS_CONTROL_REQUEST_METHOD)
        .and_then(|value| value.to_str().ok());
    if method == Method::OPTIONS && requested.is_some() {
        let requested = requested.unwrap_or_default();
        let allowed_method = config.allowed_methods.is_empty()
            || config
                .allowed_methods
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(requested));
        if !allowed_method {
            return Some((StatusCode::FORBIDDEN, "CORS method is not allowed").into_response());
        }
        let mut response = StatusCode::NO_CONTENT.into_response();
        apply_cors_headers(state, response.headers_mut(), Some(origin));
        return Some(response);
    }
    None
}

fn apply_cors_headers(state: &AppState, headers: &mut HeaderMap, origin: Option<&str>) {
    let Some(origin) = origin else {
        return;
    };
    let config = &state.config().server.cors;
    let allowed = config.allowed_origins.is_empty()
        || config
            .allowed_origins
            .iter()
            .any(|item| item == "*" || item == origin);
    if !allowed {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    if config.allow_credentials {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
    }
    if let Ok(value) = HeaderValue::from_str(&config.allowed_methods.join(", ")) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, value);
    }
    if let Ok(value) = HeaderValue::from_str(&config.allowed_headers.join(", ")) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
    }
}

fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
        })
        .map(str::trim)
        .and_then(|value| value.parse().ok())
}

fn tracing_config(state: &AppState) -> TracingHeaderConfig {
    let config = &state.config().server.trace;
    TracingHeaderConfig {
        trace_header: config.trace_header.clone(),
        request_header: config.request_header.clone(),
        thread_header: config.thread_header.clone(),
        extra_trace_headers: config.extra_trace_headers.clone(),
        extra_trace_body_fields: config.extra_trace_body_fields.clone(),
        claude_code_trace_enabled: config.claude_code_trace_enabled,
        codex_trace_enabled: config.codex_trace_enabled,
        open_code_trace_enabled: config.opencode_trace_enabled,
    }
}

fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "ar-{nanos:x}-{:x}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use tower::ServiceExt;

    use crate::app_state::AppServices;
    use crate::system_handlers::{InitializeSystemParams, SystemService};

    use super::*;

    fn wrapped_router(config: conduit_config::AppConfig) -> Router {
        wrapped_router_with_services(config, AppServices::new())
    }

    fn wrapped_router_with_services(
        config: conduit_config::AppConfig,
        services: AppServices,
    ) -> Router {
        let state = AppState::new(Arc::new(config), Arc::new(services));
        Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route("/v1/models", get(|| async { "models" }))
            .route("/openapi/v1/graphql", get(|| async { "openapi" }))
            .route("/admin/graphql", get(|| async { "admin" }))
            .route("/gateway/v1/models", get(|| async { "models" }))
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    "late"
                }),
            )
            .layer(from_fn_with_state(
                state.clone(),
                production_request_middleware,
            ))
            .with_state(state)
    }

    #[derive(Default)]
    struct BlocklistSystemService {
        blocked_ips: Vec<String>,
        read_fails: bool,
    }

    #[async_trait::async_trait]
    impl SystemService for BlocklistSystemService {
        async fn is_initialized(&self) -> Result<bool, String> {
            Ok(true)
        }

        async fn initialize(&self, _params: InitializeSystemParams) -> Result<(), String> {
            Ok(())
        }

        async fn brand_logo(&self) -> Result<String, String> {
            Ok(String::new())
        }

        async fn blocked_ips(&self) -> Result<Vec<String>, String> {
            if self.read_fails {
                Err("settings unavailable".to_string())
            } else {
                Ok(self.blocked_ips.clone())
            }
        }
    }

    fn blocklist_services(blocked_ips: &[&str]) -> AppServices {
        AppServices::new().with_system_service(Arc::new(BlocklistSystemService {
            blocked_ips: blocked_ips
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            read_fails: false,
        }))
    }

    #[tokio::test]
    async fn preflight_bypasses_routes_and_emits_cors_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let response = wrapped_router(conduit_config::AppConfig::default())
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/missing")
                    .header(header::ORIGIN, "https://ui.example")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("https://ui.example"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn request_timeout_returns_gateway_timeout_json() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut config = conduit_config::AppConfig::default();
        config.server.request_timeout = Duration::from_millis(5);
        let response = wrapped_router(config)
            .oneshot(Request::builder().uri("/slow").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = to_bytes(response.into_body(), 4096).await?;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body)?["error"]["type"],
            "timeout"
        );
        Ok(())
    }

    #[tokio::test]
    async fn blocked_ip_rejects_llm_and_openapi_routes_before_cors()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = wrapped_router_with_services(
            conduit_config::AppConfig::default(),
            blocklist_services(&["203.0.113.0/24"]),
        );

        for path in ["/v1/models", "/openapi/v1/graphql"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("x-forwarded-for", "203.0.113.9, 10.0.0.1")
                        .header(header::ORIGIN, "https://ui.example")
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path {path}");
            assert!(
                response
                    .headers()
                    .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                    .is_none()
            );
            let body = to_bytes(response.into_body(), 4096).await?;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body)?["error"]["message"],
                "IP address is blocked"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn blocked_ip_does_not_apply_to_admin_routes() -> Result<(), Box<dyn std::error::Error>> {
        let response = wrapped_router_with_services(
            conduit_config::AppConfig::default(),
            blocklist_services(&["203.0.113.9"]),
        )
        .oneshot(
            Request::builder()
                .uri("/admin/graphql")
                .header("x-real-ip", "203.0.113.9")
                .body(Body::empty())?,
        )
        .await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn blocked_ip_honors_server_base_path() -> Result<(), Box<dyn std::error::Error>> {
        let mut config = conduit_config::AppConfig::default();
        config.server.base_path = "/gateway".to_string();
        let response = wrapped_router_with_services(config, blocklist_services(&["2001:db8::/32"]))
            .oneshot(
                Request::builder()
                    .uri("/gateway/v1/models")
                    .header("x-real-ip", "2001:db8::8")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn blocklist_read_failure_fails_open() -> Result<(), Box<dyn std::error::Error>> {
        let services = AppServices::new().with_system_service(Arc::new(BlocklistSystemService {
            blocked_ips: Vec::new(),
            read_fails: true,
        }));
        let response = wrapped_router_with_services(conduit_config::AppConfig::default(), services)
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .header("x-real-ip", "203.0.113.9")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }
}

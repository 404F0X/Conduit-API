use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{Response, Uri, header};
use axum::routing::{get, post};
use tokio::net::TcpListener;

use crate::admin_handlers;
use crate::anthropic_handlers;
use crate::app_state::AppState;
use crate::asset_source::{AssetSource, FileSystemAssets};
use crate::auth_handlers;
use crate::gemini_handlers;
use crate::graphql_handlers;
use crate::health;
use crate::middleware::{RecordedMiddleware, RequestSource, middleware_order, source_for_route};
use crate::oauth_handlers;
use crate::oidc_handlers;
use crate::openai_handlers;
use crate::request_content_handlers;
use crate::request_preview_handlers;
use crate::system_handlers;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownSignalKind {
    Interrupt,
    Terminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteGroupKind {
    Admin,
    System,
    Api,
    LlmApi,
    Playground,
    Frontend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteTimeoutKind {
    Request,
    LlmRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteGroupMetadata {
    pub kind: RouteGroupKind,
    pub timeout: RouteTimeoutKind,
    pub source: RequestSource,
}

impl RouteGroupMetadata {
    pub fn timeout_duration(&self, state: &AppState) -> Duration {
        match self.timeout {
            RouteTimeoutKind::Request => state.request_timeout(),
            RouteTimeoutKind::LlmRequest => state.llm_request_timeout(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteGroupComposition {
    pub metadata: RouteGroupMetadata,
    pub middleware: &'static [RecordedMiddleware],
}

pub fn route_group_for_path(request_path: &str) -> RouteGroupMetadata {
    let path = request_path_without_query(request_path);
    let kind = if matches_path_prefix(path, "/admin") || matches_path_prefix(path, "/internal") {
        RouteGroupKind::Admin
    } else if matches_path_prefix(path, "/health")
        || matches_path_prefix(path, "/system")
        || matches_path_prefix(path, "/api/system")
    {
        RouteGroupKind::System
    } else if matches_path_prefix(path, "/playground") {
        RouteGroupKind::Playground
    } else if is_llm_api_path(path) {
        RouteGroupKind::LlmApi
    } else if is_api_group_path(path) {
        RouteGroupKind::Api
    } else {
        RouteGroupKind::Frontend
    };

    let timeout = match kind {
        RouteGroupKind::LlmApi | RouteGroupKind::Playground => RouteTimeoutKind::LlmRequest,
        RouteGroupKind::Admin
        | RouteGroupKind::System
        | RouteGroupKind::Api
        | RouteGroupKind::Frontend => RouteTimeoutKind::Request,
    };

    RouteGroupMetadata {
        kind,
        timeout,
        // Admin and system requests keep the default API source until a real
        // auth/source layer needs a more specific marker.
        source: source_for_route(kind == RouteGroupKind::Playground),
    }
}

pub fn route_group_composition_for_path(request_path: &str) -> RouteGroupComposition {
    RouteGroupComposition {
        metadata: route_group_for_path(request_path),
        middleware: middleware_order(),
    }
}

/// Lifecycle stage of the HTTP server boot.
///
/// Mirrors the Go fx start order documented in the TODO S08 contract:
/// `config -> logging -> db -> cache -> services -> scheduler -> router ->
/// server` (see `conduit/cmd/conduit/main.go` `startServer` + `server.Run`
/// in `conduit/internal/server/server.go`). The Go binary delegates stage
/// wiring to `go.uber.org/fx` which runs `OnStart` hooks in dependency
/// order; we materialise that order as a fixed `InitSequence` (see below)
/// so the Rust binary can report the failing stage for ops triage (S13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpInitStage {
    Config,
    Logging,
    Db,
    Cache,
    Services,
    Scheduler,
    Router,
    Listener,
    Server,
}

impl HttpInitStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Logging => "logging",
            Self::Db => "db",
            Self::Cache => "cache",
            Self::Services => "services",
            Self::Scheduler => "scheduler",
            Self::Router => "router",
            Self::Listener => "listener",
            Self::Server => "server",
        }
    }
}

impl std::fmt::Display for HttpInitStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct HttpInitError {
    pub stage: HttpInitStage,
    pub message: String,
}

impl HttpInitError {
    pub fn new(stage: HttpInitStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HttpInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} initialization failed: {}", self.stage, self.message)
    }
}

impl std::error::Error for HttpInitError {}

pub fn http_init_error(stage: HttpInitStage, err: impl std::fmt::Display) -> HttpInitError {
    HttpInitError::new(stage, err.to_string())
}

/// Fixed, ordered boot sequence for the HTTP server.
///
/// Materialises the S08 contract `config -> logging -> db -> cache ->
/// services -> scheduler -> router -> server` so that the binary walks
/// stages in a deterministic order and can surface the failing stage via
/// `stage_error` (S13). The Go side encodes this implicitly through
/// `go.uber.org/fx` dependency injection in `server.Run`
/// (`conduit/internal/server/server.go:81-125`) — we make it explicit and
/// introspectable. `next_stage` lets a driver loop advance one stage at a
/// time and stop at the first error, matching fx's
/// start-all-or-roll-back semantics.
///
/// `Listener` is intentionally absent from the boot sequence: it is the
/// final *bind* step before `Server` (the run loop), but the S08 contract
/// ends at `server`. `InitSequence::Server` covers both the listener bind
/// and the serve loop in the Go model (`server.Run` does
/// `ListenAndServe`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InitSequence {
    cursor: usize,
}

impl InitSequence {
    /// Full ordered list of stages, matching the S08 contract.
    pub const STAGES: [HttpInitStage; 8] = [
        HttpInitStage::Config,
        HttpInitStage::Logging,
        HttpInitStage::Db,
        HttpInitStage::Cache,
        HttpInitStage::Services,
        HttpInitStage::Scheduler,
        HttpInitStage::Router,
        HttpInitStage::Server,
    ];

    /// Create a sequence positioned before the first stage.
    pub fn new() -> Self {
        Self::default()
    }

    /// Current stage, if any. `None` once the sequence has been fully
    /// consumed past the last stage.
    pub fn current(self) -> Option<HttpInitStage> {
        Self::STAGES.get(self.cursor).copied()
    }

    /// Advance to and return the next stage, or `None` if the sequence is
    /// already past the last stage. Stage transitions are monotonic and
    /// never skip.
    pub fn next_stage(&mut self) -> Option<HttpInitStage> {
        if self.cursor >= Self::STAGES.len() {
            return None;
        }
        self.cursor += 1;
        Self::STAGES.get(self.cursor).copied()
    }

    /// Stages that have already completed (everything strictly before the
    /// cursor). Useful for emitting a "stage N failed after M stages OK"
    /// triage line.
    pub fn completed(self) -> &'static [HttpInitStage] {
        let end = self.cursor.min(Self::STAGES.len());
        &Self::STAGES[..end]
    }

    /// Whether the entire sequence has been consumed.
    pub fn is_done(self) -> bool {
        self.cursor >= Self::STAGES.len()
    }

    /// Reset the cursor back to before the first stage.
    pub fn reset(&mut self) {
        self.cursor = 0;
    }
}

/// Build a [`StagedInitError`] carrying the failing stage and a human
/// triage hint listing already-completed stages (S13).
///
/// Pure: does not perform I/O or logging. The caller (the binary) is
/// responsible for emitting the message. Mirrors the Go behavior where
/// fx reports the failing provider but does not natively list prior
/// completed stages — we add that for ops triage.
pub fn stage_error(stage: HttpInitStage, cause: impl std::fmt::Display) -> StagedInitError {
    StagedInitError::new(stage, cause)
}

/// Enriched init error that remembers the failing stage and a slice of
/// already-completed stages for ops triage (S13).
///
/// This is the structured companion to [`HttpInitError`]; it carries the
/// same stage identifier plus the prior-stage context so logs can say
/// "db failed after config, logging" instead of just "db failed".
#[derive(Debug, PartialEq, Eq)]
pub struct StagedInitError {
    pub stage: HttpInitStage,
    pub message: String,
    pub completed_stages: &'static [HttpInitStage],
}

impl StagedInitError {
    pub fn new(stage: HttpInitStage, cause: impl std::fmt::Display) -> Self {
        Self {
            stage,
            message: cause.to_string(),
            completed_stages: prior_stages_for(stage),
        }
    }

    /// Stable, single-line form suitable for an ops log line:
    /// `"<stage> initialization failed: <message> (after: <prior stages>)"`.
    pub fn triage_line(&self) -> String {
        let prior = if self.completed_stages.is_empty() {
            "none".to_string()
        } else {
            self.completed_stages
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "{} initialization failed: {} (after: {})",
            self.stage, self.message, prior
        )
    }
}

impl std::fmt::Display for StagedInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.triage_line())
    }
}

impl std::error::Error for StagedInitError {}

/// Return the slice of stages that should have completed before `stage`
/// in the canonical [`InitSequence::STAGES`] order. Returns an empty
/// slice for stages not on the S08 contract path.
fn prior_stages_for(stage: HttpInitStage) -> &'static [HttpInitStage] {
    let end = InitSequence::STAGES
        .iter()
        .position(|s| *s == stage)
        .unwrap_or(0);
    &InitSequence::STAGES[..end]
}

/// Configuration for a graceful shutdown drain (S07/S10).
///
/// The Go side implements this via Go stdlib `http.Server.Shutdown(ctx)`
/// (`conduit/internal/server/server.go:77-79`) where `ctx` carries the
/// deadline set by `fx.StopTimeout(30*time.Second)`
/// (`conduit/cmd/conduit/main.go:62`). On shutdown the stdlib server
/// stops accepting new connections, waits for in-flight handlers to
/// finish, and force-closes when the context expires. We mirror that as
/// pure config: a bounded drain timeout plus a soft cap on in-flight
/// work that, if exceeded, should trigger a force exit (the cap is
/// advisory and consumed by the runner, not enforced here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownPlan {
    /// Maximum time to wait for in-flight requests to drain before
    /// force-closing. Mirrors `fx.StopTimeout`. Default 30s.
    pub drain_timeout: Duration,
    /// Advisory cap on in-flight work. If the runner observes more
    /// in-flight requests than this after `drain_timeout`, it logs and
    /// force-exits. Set to `usize::MAX` to disable the cap. The Go side
    /// does not impose an explicit in-flight bound; we add one so the
    /// Rust binary can guarantee exit bounded by O(cap) cancellations.
    pub in_flight_bound: usize,
}

impl ShutdownPlan {
    /// Go-canonical default: 30s drain timeout, no explicit in-flight
    /// bound (matches `fx.StopTimeout(30*time.Second)` and Go stdlib
    /// `Shutdown` semantics).
    pub const DEFAULT: Self = Self {
        drain_timeout: Duration::from_secs(30),
        in_flight_bound: usize::MAX,
    };

    /// Builder: set the drain timeout.
    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Builder: set the in-flight advisory cap.
    pub fn with_in_flight_bound(mut self, bound: usize) -> Self {
        self.in_flight_bound = bound;
        self
    }

    /// Whether the in-flight advisory cap is enabled (non-max).
    pub fn has_in_flight_cap(self) -> bool {
        self.in_flight_bound != usize::MAX
    }
}

impl Default for ShutdownPlan {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The kind of signal that triggered shutdown, plus the resolved plan.
///
/// `ShutdownSignalKind` (above) is kept as a small pure enum for callers
/// that only need the signal identity; `ShutdownSignal` bundles it with
/// the [`ShutdownPlan`] so the runner has everything it needs in one
/// value. Pure data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownSignal {
    pub kind: ShutdownSignalKind,
    pub plan: ShutdownPlan,
}

impl ShutdownSignal {
    pub fn new(kind: ShutdownSignalKind, plan: ShutdownPlan) -> Self {
        Self { kind, plan }
    }
}

/// Resolve a single shutdown signal from two candidate futures (SIGINT
/// and SIGTERM style), then attach the given plan (S07/S10).
///
/// This is the small pure wrapper around [`shutdown_signal_from`] that
/// encodes the tokio::select-or behavior for the two POSIX signals and
/// returns a fully populated [`ShutdownSignal`] ready for the runner.
/// Mirrors the Go binary, which shuts down on whichever of SIGINT /
/// SIGTERM arrives first via fx signal handling.
pub async fn select_shutdown_signal<I, T>(
    interrupt: I,
    terminate: T,
    plan: ShutdownPlan,
) -> ShutdownSignal
where
    I: Future<Output = ()> + Send,
    T: Future<Output = ()> + Send,
{
    let kind = shutdown_signal_from(interrupt, terminate).await;
    ShutdownSignal::new(kind, plan)
}

pub fn build_router(state: AppState) -> Router {
    build_router_with_asset_source(state, crate::asset_source::production_asset_source())
}

pub fn build_router_with_static_root(state: AppState, static_root: impl Into<PathBuf>) -> Router {
    build_router_with_asset_source(state, Box::new(FileSystemAssets::new(static_root.into())))
}

pub fn build_router_with_asset_source(
    state: AppState,
    asset_source: Box<dyn AssetSource>,
) -> Router {
    let _composition = route_group_composition_for_path("/health");

    // P1-003 S11/S16: when `server.base_path` is non-empty (and not just "/"),
    // every API route + the static fallback mount under that prefix and the
    // root-level `/health` is suppressed unless the compat flag re-enables it.
    // Go declares `BasePath` but never wires it; the Rust side implements the
    // S11/S16 compat strategy explicitly (see `model.rs:520-521` comment).
    let base_path = state.base_path().to_string();

    let mut router = Router::new();

    // Health follows the same mount contract as every other endpoint. With a
    // non-empty base path it is available at `<base_path>/health`; the legacy
    // root route remains suppressed unless a future compatibility flag opts in.
    router = router.route(&mount_path(&base_path, "/health"), get(health::health));

    // ===== Public routes (no auth required) =====
    // Go routes.go:76-89 — "System Status and Initialize - DO NOT AUTH",
    // "User Login - DO NOT AUTH", "Favicon API - DO NOT AUTH".
    router = router
        .route(
            &mount_path(&base_path, "/api/system/version"),
            get(admin_handlers::system_version),
        )
        .route(
            &mount_path(&base_path, "/admin/system/status"),
            get(system_handlers::get_system_status),
        )
        .route(
            &mount_path(&base_path, "/admin/system/initialize"),
            post(system_handlers::initialize_system),
        )
        .route(
            &mount_path(&base_path, "/admin/auth/signin"),
            post(auth_handlers::sign_in),
        )
        .route(
            &mount_path(&base_path, "/admin/auth/signup"),
            post(auth_handlers::sign_up),
        )
        .route(
            &mount_path(&base_path, "/favicon"),
            get(system_handlers::get_favicon),
        )
        // OIDC public routes (no auth) — Go routes.go:91-94.
        .route(
            &mount_path(&base_path, "/oauth/oidc/providers"),
            get(oidc_handlers::get_providers),
        )
        .route(
            &mount_path(&base_path, "/oauth/oidc/authorize/{provider}"),
            get(oidc_handlers::get_authorize_url),
        )
        .route(
            &mount_path(&base_path, "/oauth/oidc/callback"),
            get(oidc_handlers::callback),
        )
        .route(
            &mount_path(&base_path, "/oauth/oidc/callback/{provider}"),
            get(oidc_handlers::callback_with_provider),
        )
        .route(
            &mount_path(&base_path, "/oauth/oidc/exchange"),
            post(oidc_handlers::exchange),
        );

    // ===== JWT-protected admin routes =====
    // Go routes.go:96-139 — adminGroup with `middleware.WithJWTAuth`.
    let admin_protected = Router::new()
        .route(
            &mount_path(&base_path, "/admin/oidc/link/{provider}"),
            get(oidc_handlers::get_link_authorize_url),
        )
        .route(
            &mount_path(&base_path, "/admin/{provider}/oauth/start"),
            post(oauth_handlers::start_oauth),
        )
        .route(
            &mount_path(&base_path, "/admin/{provider}/oauth/exchange"),
            post(oauth_handlers::exchange),
        )
        .route(
            &mount_path(&base_path, "/admin/copilot/oauth/poll"),
            post(oauth_handlers::poll_oauth),
        )
        .route(
            &mount_path(&base_path, "/admin/codex/auth/decode"),
            post(oauth_handlers::decode_auth_json),
        )
        .route(
            &mount_path(&base_path, "/admin/requests/{request_id}/content"),
            get(request_content_handlers::download_request_content),
        )
        .route(
            &mount_path(&base_path, "/admin/requests/{request_id}/preview"),
            get(request_preview_handlers::preview_request),
        )
        .route(
            &mount_path(&base_path, "/admin/graphql"),
            post(graphql_handlers::graphql_handler),
        )
        .route(
            &mount_path(&base_path, "/admin/playground"),
            get(graphql_handlers::graphql_playground),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::jwt_auth::jwt_admin_auth,
        ));
    router = router.merge(admin_protected);

    // Service-account-only management API. Authentication runs first and the
    // group-level authorization layer then rejects every non-service-account
    // key before any handler executes.
    let openapi_graphql = Router::new()
        .route(
            &mount_path(&base_path, "/openapi/v1/graphql"),
            post(crate::openapi_graphql_handlers::graphql_handler),
        )
        // Go `openAPIGroup.POST("/webhook/echo", handlers.System.WebhookEcho)`
        // (routes.go:156) — same service-account-authed group as the GraphQL
        // endpoint. The handler already existed but was never routed (P-52).
        .route(
            &mount_path(&base_path, "/openapi/webhook/echo"),
            post(crate::webhook_handlers::webhook_echo),
        )
        .route_layer(axum::middleware::from_fn(
            crate::middleware::api_key_auth::require_service_account,
        ))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::api_key_auth::api_key_auth,
        ));
    router = router.merge(openapi_graphql);

    // System-wide automation API. It reuses the complete admin GraphQL schema
    // but has a deliberately separate authentication boundary: only a
    // service-account key with the dedicated `system:admin` scope is promoted
    // to administrator authority by the handler.
    let internal_admin_graphql = Router::new()
        .route(
            &mount_path(&base_path, "/internal/v1/graphql"),
            post(graphql_handlers::internal_graphql_handler),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::api_key_auth::api_key_auth,
        ));
    router = router.merge(internal_admin_graphql);

    // ===== LLM API routes (API key auth) =====
    // Go routes.go:155-180 — llmAPIGroup with `middleware.WithAPIKeyAuth`.
    let llm_uploads = Router::new()
        .route(
            &mount_path(&base_path, "/v1/images/edits"),
            post(openai_handlers::create_image_edit),
        )
        .route(
            &mount_path(&base_path, "/v1/audio/transcriptions"),
            post(openai_handlers::create_transcription),
        )
        .route(
            &mount_path(&base_path, "/v1/audio/translations"),
            post(openai_handlers::create_translation),
        )
        .layer(DefaultBodyLimit::max(
            openai_handlers::MULTIPART_BODY_LIMIT_BYTES,
        ));
    let llm_api = Router::new()
        .route(
            &mount_path(&base_path, "/v1/models"),
            get(openai_handlers::list_models),
        )
        .route(
            // Catch-all (axum 0.8 `{*model}`) to match Go's gin catch-all
            // `/models/*model` (openai.go:677): model ids can contain slashes
            // (e.g. `deepseek/deepseek-chat`, `meta-llama/Llama-3-70b`). The
            // handler already strips the leading slash via `trim_model_splat`.
            // A single-segment `{model}` 404s such ids unless URL-encoded (P-52).
            &mount_path(&base_path, "/v1/models/{*model}"),
            get(openai_handlers::retrieve_model),
        )
        .route(
            &mount_path(&base_path, "/anthropic/v1/models"),
            get(anthropic_handlers::list_models),
        )
        .route(
            &mount_path(&base_path, "/v1beta/models"),
            get(gemini_handlers::list_models),
        )
        // Gemini generateContent / streamGenerateContent (Go: gemini.go:66)
        .route(
            &mount_path(&base_path, "/v1beta/models/{model_action}"),
            post(gemini_handlers::generate_content),
        )
        .route(
            &mount_path(&base_path, "/gemini/{gemini_api_version}/models"),
            get(gemini_handlers::list_models),
        )
        .route(
            &mount_path(
                &base_path,
                "/gemini/{gemini_api_version}/models/{model_action}",
            ),
            post(gemini_handlers::generate_content),
        )
        .route(
            &mount_path(&base_path, "/v1/messages"),
            post(anthropic_handlers::create_message),
        )
        .route(
            &mount_path(&base_path, "/v1/messages/count_tokens"),
            post(anthropic_handlers::count_message_tokens),
        )
        .route(
            &mount_path(&base_path, "/anthropic/v1/messages"),
            post(anthropic_handlers::create_message),
        )
        .route(
            &mount_path(&base_path, "/anthropic/v1/messages/count_tokens"),
            post(anthropic_handlers::count_message_tokens),
        )
        .route(
            &mount_path(&base_path, "/v1/chat/completions"),
            post(openai_handlers::create_chat_completion),
        )
        .route(
            &mount_path(&base_path, "/v1/responses"),
            post(openai_handlers::create_response),
        )
        .route(
            &mount_path(&base_path, "/v1/completions"),
            post(openai_handlers::create_completion),
        )
        .route(
            &mount_path(&base_path, "/v1/responses/compact"),
            post(openai_handlers::create_compact_response),
        )
        .route(
            &mount_path(&base_path, "/v1/rerank"),
            post(openai_handlers::create_jina_rerank),
        )
        .route(
            &mount_path(&base_path, "/jina/v1/rerank"),
            post(openai_handlers::create_jina_rerank),
        )
        .route(
            &mount_path(&base_path, "/jina/v1/embeddings"),
            post(openai_handlers::create_jina_embedding),
        )
        .route(
            &mount_path(&base_path, "/doubao/v3/contents/generations/tasks"),
            post(openai_handlers::create_doubao_task),
        )
        .route(
            &mount_path(&base_path, "/doubao/v3/contents/generations/tasks/{id}"),
            get(openai_handlers::get_video).delete(openai_handlers::delete_video),
        )
        .route(
            &mount_path(&base_path, "/v1/embeddings"),
            post(openai_handlers::create_embedding),
        )
        .route(
            &mount_path(&base_path, "/v1/audio/speech"),
            post(openai_handlers::create_speech),
        )
        .route(
            &mount_path(&base_path, "/v1/videos"),
            post(openai_handlers::create_video),
        )
        .route(
            &mount_path(&base_path, "/v1/videos/{id}"),
            get(openai_handlers::get_video).delete(openai_handlers::delete_video),
        )
        .route(
            &mount_path(&base_path, "/v1/images/generations"),
            post(openai_handlers::create_image),
        )
        .merge(llm_uploads)
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::api_key_auth::api_key_auth,
        ));
    router = router.merge(llm_api);

    let asset_source: Arc<dyn AssetSource> = Arc::from(asset_source);
    router = router
        .fallback(asset_fallback_handler)
        .layer(Extension(asset_source));

    // RUST-P11-003 — inject the admin GraphQL schema as an Extension so the
    // `graphql_handler` can extract it. If the host wired a schema with service
    // data (via AppServices::with_admin_schema), use that; otherwise fall back
    // to the bare schema (introspection + health/version only).
    let admin_schema = state
        .services()
        .admin_schema()
        .cloned()
        .unwrap_or_else(conduit_admin_graphql::build_admin_schema);
    router = router.layer(Extension(admin_schema));

    // P1-002 S14: install a panic-catching layer so a handler panic is caught
    // and converted to a 500 `internal_error` JSON response (mirroring Go's
    // `defer recover()`). `catch_unwind` is a safe stdlib API and is not gated
    // by the workspace `unsafe_code = "forbid"` lint.
    router = router
        .layer(crate::panic_layer::PanicCatchLayer::new())
        .layer(axum::middleware::from_fn(
            crate::middleware::metrics::inject_metrics_state,
        ))
        .layer(Extension(state.metrics().clone()))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::runtime::production_request_middleware,
        ));

    router.with_state(state)
}

pub fn metrics_router(state: crate::middleware::metrics::MetricsState, path: &str) -> Router {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Router::new()
        .route(
            &path,
            get(
                |Extension(state): Extension<crate::middleware::metrics::MetricsState>| async move {
                    let snapshot = state.snapshot();
                    (
                        [(
                            header::CONTENT_TYPE,
                            "text/plain; version=0.0.4; charset=utf-8",
                        )],
                        format!(
                            "# HELP conduit_http_requests_total Total HTTP requests.\n\
# TYPE conduit_http_requests_total counter\n\
conduit_http_requests_total {}\n\
# HELP conduit_http_requests_in_flight Current in-flight HTTP requests.\n\
# TYPE conduit_http_requests_in_flight gauge\n\
conduit_http_requests_in_flight {}\n",
                            snapshot.request_count, snapshot.in_flight
                        ),
                    )
                },
            ),
        )
        .layer(Extension(state))
}

fn is_llm_api_path(path: &str) -> bool {
    [
        "/v1",
        "/v1beta",
        "/anthropic",
        "/gemini",
        "/jina",
        "/doubao",
    ]
    .iter()
    .any(|prefix| matches_path_prefix(path, prefix))
}

fn is_api_group_path(path: &str) -> bool {
    ["/api", "/oauth", "/openapi"]
        .iter()
        .any(|prefix| matches_path_prefix(path, prefix))
}

fn matches_path_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn request_path_without_query(request_path: &str) -> &str {
    request_path
        .split_once('?')
        .map_or(request_path, |(path, _)| path)
}

pub fn strip_base_path_for_mount<'a>(request_path: &'a str, base_path: &str) -> Option<&'a str> {
    let base_path = base_path.trim_end_matches('/');
    if base_path.is_empty() {
        return Some(request_path);
    }

    if request_path == base_path {
        return Some("/");
    }

    // Require a path-segment boundary so "/gatewayish" is not accepted for a
    // "/gateway" mount.
    request_path
        .strip_prefix(base_path)
        .filter(|stripped| stripped.starts_with('/'))
}

/// Validate a `server.base_path` value before it is used to mount routes.
///
/// Mirrors the Go semantics in `crates/conduit-config/src/validate.rs` and the
/// intent of TODO RUST-P1-003 S11/S16: the value must be empty, or start with
/// `/`, must not carry a trailing `/` (except for the root `/` itself), and must
/// be URL-safe (no query/fragment). Returns the first failure as an owned
/// `String` so callers can surface it without pulling a cross-crate error type.
///
/// Note: the Go source (`internal/server/config.go`) declares `BasePath` and
/// defaults it to `""` but never actually wires it into route mounting; this
/// validator encodes the contract the Rust side is obligated to enforce
/// (S11/S16) once base_path becomes non-empty.
pub fn validate_base_path(base_path: &str) -> Result<(), String> {
    if base_path.is_empty() {
        return Ok(());
    }
    if !base_path.starts_with('/') {
        return Err("server.base_path must be empty or start with '/'".to_string());
    }
    // Root "/" is the only value permitted to end with '/'.
    if base_path.len() > 1 && base_path.ends_with('/') {
        return Err("server.base_path must not end with '/'".to_string());
    }
    if base_path.contains('?') || base_path.contains('#') {
        return Err("server.base_path must not contain query or fragment".to_string());
    }
    // URL-safety: reject characters that would need percent-encoding inside a
    // path segment. Allow unreserved characters plus `/` separators.
    for ch in base_path.chars() {
        let safe = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~' | '/');
        if !safe {
            return Err(format!(
                "server.base_path must be URL-safe (unexpected character {ch:?})"
            ));
        }
    }
    Ok(())
}

/// Resolve the final mount path for a route under a `server.base_path`.
///
/// Rules (mirror S11/S16 + Go intent):
/// * empty `base_path` → route passes through unchanged;
/// * `base_path == "/"` → route passes through unchanged (root is a no-op);
/// * route already prefixed with `base_path` → returned unchanged (idempotent);
/// * otherwise the base and route are joined with exactly one `/` separator,
///   collapsing any accidental doubling.
///
/// `route` is expected to already start with `/` (the Conduit API router only
/// registers absolute paths). A `route` of `"/"` yields `base_path` itself.
pub fn mount_path(base_path: &str, route: &str) -> String {
    let trimmed_base = base_path.trim_end_matches('/');
    if trimmed_base.is_empty() || trimmed_base == "/" {
        return route.to_string();
    }
    if strip_base_path_for_mount(route, trimmed_base).is_some() {
        // Idempotent: route is already mounted under the base path.
        return route.to_string();
    }
    let route = if route == "/" { "" } else { route };
    format!("{trimmed_base}{route}")
}

/// Decide whether the root-level `/health` route should be exposed.
///
/// Per S16: when `server.base_path` is non-empty, the root `/health` MUST NOT
/// be separately exposed unless a compatibility/operations flag explicitly
/// opts back in. An empty base path always exposes `/health` at root (current
/// default behavior, matching Go `routes.go` line 79).
pub fn should_expose_root_health(base_path: &str, compat_flag: bool) -> bool {
    let trimmed = base_path.trim_end_matches('/');
    trimmed.is_empty() || compat_flag
}

async fn asset_fallback_handler(
    State(state): State<AppState>,
    uri: Uri,
    Extension(source): Extension<Arc<dyn AssetSource>>,
) -> Response<Body> {
    let Some(path) = strip_base_path_for_mount(uri.path(), state.base_path()) else {
        return crate::static_files::api_not_found_response();
    };
    crate::static_files::serve_from_asset_source_with_base_path(
        path,
        state.base_path(),
        source.as_ref(),
    )
}

pub async fn serve_listener<S>(
    listener: TcpListener,
    state: AppState,
    shutdown: S,
) -> std::io::Result<()>
where
    S: Future<Output = ()> + Send + 'static,
{
    axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}

/// Serve until a shutdown signal arrives, then give active connections a
/// bounded window to drain. When the deadline expires the server future is
/// dropped and a timed-out error is returned, preventing an indefinite deploy
/// or process-stop hang on stalled streaming clients.
pub async fn serve_listener_with_graceful_timeout<S>(
    listener: TcpListener,
    state: AppState,
    shutdown: S,
    graceful_timeout: Duration,
) -> std::io::Result<()>
where
    S: Future<Output = ()> + Send,
{
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(
        listener,
        build_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = drain_rx.await;
    })
    .into_future();
    tokio::pin!(server);
    tokio::pin!(shutdown);

    tokio::select! {
        result = &mut server => result,
        _ = &mut shutdown => {
            let _ = drain_tx.send(());
            match tokio::time::timeout(graceful_timeout, &mut server).await {
                Ok(result) => result,
                Err(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "HTTP graceful shutdown exceeded {} ms",
                        graceful_timeout.as_millis()
                    ),
                )),
            }
        }
    }
}

pub async fn shutdown_signal() -> ShutdownSignalKind {
    shutdown_signal_from(ctrl_c_signal(), terminate_signal()).await
}

pub async fn shutdown_signal_from<I, T>(interrupt: I, terminate: T) -> ShutdownSignalKind
where
    I: Future<Output = ()> + Send,
    T: Future<Output = ()> + Send,
{
    tokio::pin!(interrupt);
    tokio::pin!(terminate);

    tokio::select! {
        _ = &mut interrupt => ShutdownSignalKind::Interrupt,
        _ = &mut terminate => ShutdownSignalKind::Terminate,
    }
}

async fn ctrl_c_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(unix)]
async fn terminate_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    match signal(SignalKind::terminate()) {
        Ok(mut signal) => {
            let _ = signal.recv().await;
        }
        Err(_) => std::future::pending::<()>().await,
    }
}

#[cfg(not(unix))]
async fn terminate_signal() {
    std::future::pending::<()>().await
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode, header};
    use conduit_config::AppConfig;
    use serde_json::Value;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;
    use tokio::time::timeout;
    use tower::Service;

    use crate::static_files::{StaticFallbackDecision, decide_static_fallback};

    use super::*;

    /// Permissive validator so routing tests reach the handler past the
    /// fail-closed `api_key_auth` guard (P-24). Accepts any key.
    struct AlwaysValidApiKey;

    #[async_trait::async_trait]
    impl crate::middleware::api_key_auth::ApiKeyValidationService for AlwaysValidApiKey {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<
            crate::middleware::api_key_auth::ValidatedApiKeyMetadata,
            crate::middleware::api_key_auth::ApiKeyValidationError,
        > {
            Ok(crate::middleware::api_key_auth::ValidatedApiKeyMetadata::default())
        }
    }

    struct FixedApiKeyMetadata(crate::middleware::api_key_auth::ValidatedApiKeyMetadata);

    #[async_trait::async_trait]
    impl crate::middleware::api_key_auth::ApiKeyValidationService for FixedApiKeyMetadata {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<
            crate::middleware::api_key_auth::ValidatedApiKeyMetadata,
            crate::middleware::api_key_auth::ApiKeyValidationError,
        > {
            Ok(self.0.clone())
        }
    }

    /// `AppState` with only the permissive test validator wired — the analogue
    /// of `AppState::default()` for routing tests that send a key.
    fn state_with_validator() -> AppState {
        let services = crate::app_state::AppServices::new()
            .with_api_key_validation_service(std::sync::Arc::new(AlwaysValidApiKey));
        AppState::new(
            std::sync::Arc::new(conduit_config::AppConfig::default()),
            std::sync::Arc::new(services),
        )
    }

    fn state_with_metadata(
        metadata: crate::middleware::api_key_auth::ValidatedApiKeyMetadata,
    ) -> AppState {
        let services = crate::app_state::AppServices::new()
            .with_api_key_validation_service(std::sync::Arc::new(FixedApiKeyMetadata(metadata)));
        AppState::new(
            std::sync::Arc::new(conduit_config::AppConfig::default()),
            std::sync::Arc::new(services),
        )
    }

    #[tokio::test]
    async fn production_router_updates_shared_metrics_state() -> Result<(), Box<dyn Error>> {
        // Metrics now default to disabled (P-45); this test exercises the
        // counting path, so enable them explicitly rather than relying on the
        // default.
        let mut config = AppConfig::default();
        config.metrics.enabled = true;
        let state = AppState::from_config(config);
        let metrics = state.metrics().clone();
        let mut app = build_router(state);
        let response = app
            .call(Request::builder().uri("/health").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(metrics.snapshot().request_count, 1);
        assert_eq!(metrics.snapshot().in_flight, 0);
        Ok(())
    }

    #[tokio::test]
    async fn metrics_router_exposes_prometheus_text() -> Result<(), Box<dyn Error>> {
        let state = crate::middleware::metrics::MetricsState::new(true);
        state
            .request_count
            .store(12, std::sync::atomic::Ordering::Relaxed);
        let mut app = metrics_router(state, "/custom-metrics");
        let response = app
            .call(
                Request::builder()
                    .uri("/custom-metrics")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await?;
        let text = std::str::from_utf8(&body)?;
        assert!(text.contains("conduit_http_requests_total 12"));
        Ok(())
    }

    #[test]
    fn route_groups_select_expected_timeout_and_source() {
        let cases = [
            (
                "/admin/users",
                RouteGroupKind::Admin,
                RouteTimeoutKind::Request,
                RequestSource::SourceAPI,
            ),
            (
                "/health",
                RouteGroupKind::System,
                RouteTimeoutKind::Request,
                RequestSource::SourceAPI,
            ),
            (
                "/api/system/version",
                RouteGroupKind::System,
                RouteTimeoutKind::Request,
                RequestSource::SourceAPI,
            ),
            (
                "/v1/chat/completions",
                RouteGroupKind::LlmApi,
                RouteTimeoutKind::LlmRequest,
                RequestSource::SourceAPI,
            ),
            (
                "/v1/models",
                RouteGroupKind::LlmApi,
                RouteTimeoutKind::LlmRequest,
                RequestSource::SourceAPI,
            ),
            (
                "/anthropic/v1/models",
                RouteGroupKind::LlmApi,
                RouteTimeoutKind::LlmRequest,
                RequestSource::SourceAPI,
            ),
            (
                "/gemini/v1/models/foo",
                RouteGroupKind::LlmApi,
                RouteTimeoutKind::LlmRequest,
                RequestSource::SourceAPI,
            ),
            (
                "/playground/chat",
                RouteGroupKind::Playground,
                RouteTimeoutKind::LlmRequest,
                RequestSource::SourcePlayground,
            ),
            (
                "/dashboard/projects/1",
                RouteGroupKind::Frontend,
                RouteTimeoutKind::Request,
                RequestSource::SourceAPI,
            ),
        ];

        for (path, kind, timeout, source) in cases {
            let metadata = route_group_for_path(path);

            assert_eq!(metadata.kind, kind, "{path}");
            assert_eq!(metadata.timeout, timeout, "{path}");
            assert_eq!(metadata.source, source, "{path}");
        }
    }

    #[test]
    fn route_group_timeout_duration_uses_state_config() {
        let mut config = AppConfig::default();
        config.server.request_timeout = Duration::from_secs(7);
        config.server.llm_request_timeout = Duration::from_secs(70);
        let state = AppState::from_config(config);

        assert_eq!(
            route_group_for_path("/admin/users").timeout_duration(&state),
            Duration::from_secs(7)
        );
        assert_eq!(
            route_group_for_path("/v1/chat/completions").timeout_duration(&state),
            Duration::from_secs(70)
        );
    }

    #[test]
    fn route_group_composition_carries_middleware_order() {
        let composition = route_group_composition_for_path("/playground/chat");

        assert_eq!(composition.metadata.kind, RouteGroupKind::Playground);
        assert_eq!(composition.middleware, middleware_order());
    }

    #[test]
    fn internal_graphql_uses_admin_route_policy() {
        assert_eq!(
            route_group_for_path("/internal/v1/graphql").kind,
            RouteGroupKind::Admin
        );
    }

    #[tokio::test]
    async fn openapi_webhook_rejects_authenticated_user_keys() -> Result<(), Box<dyn Error>> {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            project_id: 1,
            key_type: "user".to_string(),
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let response = app
            .call(
                Request::builder()
                    .method(Method::POST)
                    .uri("/openapi/webhook/echo")
                    .header(header::AUTHORIZATION, "Bearer test-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"event":"test"}"#))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn openapi_webhook_accepts_service_accounts_without_reflecting_credentials()
    -> Result<(), Box<dyn Error>> {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            project_id: 1,
            key_type: "service_account".to_string(),
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let response = app
            .call(
                Request::builder()
                    .method(Method::POST)
                    .uri("/openapi/webhook/echo?topic=hello%20world")
                    .header(header::AUTHORIZATION, "Bearer test-key")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("x-request-id", "request-7")
                    .body(Body::from(r#"{"event":"test"}"#))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        assert_eq!(body["query"]["topic"], serde_json::json!(["hello world"]));
        assert_eq!(
            body["headers"]["X-Request-Id"],
            serde_json::json!(["request-7"])
        );
        assert!(body["headers"].get("Authorization").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn internal_graphql_requires_dedicated_system_admin_scope() -> Result<(), Box<dyn Error>>
    {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            project_id: 1,
            key_type: "service_account".to_string(),
            scopes: vec![conduit_auth::scopes::slug::WRITE_USERS.to_string()],
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/internal/v1/graphql")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query":"{ version }"}"#))?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn internal_graphql_rejects_non_service_account_even_with_system_admin_scope()
    -> Result<(), Box<dyn Error>> {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            project_id: 1,
            key_type: "user".to_string(),
            scopes: vec![conduit_auth::scopes::slug::SYSTEM_ADMIN.to_string()],
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/internal/v1/graphql")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query":"{ version }"}"#))?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn internal_graphql_accepts_system_admin_service_account() -> Result<(), Box<dyn Error>> {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            api_key_name: "automation".to_string(),
            project_id: 1,
            key_type: "service_account".to_string(),
            scopes: vec![conduit_auth::scopes::slug::SYSTEM_ADMIN.to_string()],
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/internal/v1/graphql")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"query":"{ version }"}"#))?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn internal_graphql_exposes_documented_management_contract() -> Result<(), Box<dyn Error>>
    {
        let metadata = crate::middleware::api_key_auth::ValidatedApiKeyMetadata {
            api_key_id: 7,
            api_key_name: "automation".to_string(),
            project_id: 1,
            key_type: "service_account".to_string(),
            scopes: vec![conduit_auth::scopes::slug::SYSTEM_ADMIN.to_string()],
            ..Default::default()
        };
        let mut app = build_router(state_with_metadata(metadata));
        let query = r#"
          query InternalContract {
            mutationRoot: __type(name: "MutationRoot") { fields { name } }
            assignment: __type(name: "AssignUserSubscriptionInput") {
              inputFields { name type { kind name ofType { kind name } } }
            }
            profile: __type(name: "APIKeyProfileInput") {
              inputFields { name }
            }
          }
        "#;
        let request = Request::builder()
            .method(Method::POST)
            .uri("/internal/v1/graphql")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "query": query }).to_string(),
            ))?;

        let response = app.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await?)?;
        assert!(body.get("errors").is_none(), "introspection failed: {body}");

        let mutation_names = body["data"]["mutationRoot"]["fields"]
            .as_array()
            .ok_or("mutation fields missing")?
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect::<std::collections::HashSet<_>>();
        for required in [
            "updateUser",
            "createSimpleGroup",
            "assignUserSubscription",
            "grantProjectCredit",
            "updateAPIKeyProfiles",
        ] {
            assert!(mutation_names.contains(required), "missing {required}");
        }

        let input_names =
            |type_name: &str| -> Result<std::collections::HashSet<&str>, Box<dyn Error>> {
                Ok(body["data"][type_name]["inputFields"]
                    .as_array()
                    .ok_or("input fields missing")?
                    .iter()
                    .filter_map(|field| field["name"].as_str())
                    .collect())
            };
        assert!(input_names("assignment")?.contains("projectID"));
        assert!(input_names("assignment")?.contains("idempotencyKey"));
        let assignment_idempotency_key = body["data"]["assignment"]["inputFields"]
            .as_array()
            .ok_or("assignment input fields missing")?
            .iter()
            .find(|field| field["name"] == "idempotencyKey")
            .ok_or("assignment idempotencyKey missing")?;
        assert_eq!(assignment_idempotency_key["type"]["kind"], "NON_NULL");
        assert_eq!(
            assignment_idempotency_key["type"]["ofType"]["name"],
            "String"
        );
        assert!(input_names("profile")?.contains("maxConcurrentRequests"));
        Ok(())
    }

    #[test]
    fn http_init_error_records_stage_and_message() {
        let err = http_init_error(HttpInitStage::Router, "missing base route");

        assert_eq!(err.stage, HttpInitStage::Router);
        assert_eq!(err.message, "missing base route");
        assert_eq!(
            err.to_string(),
            "router initialization failed: missing base route"
        );
        assert_eq!(HttpInitStage::Db.as_str(), "db");
    }

    #[tokio::test]
    async fn health_route_returns_ok() -> Result<(), Box<dyn Error>> {
        let mut app = build_router(AppState::default());
        let request = Request::builder().uri("/health").body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"status\":\"ok\""));

        Ok(())
    }

    #[tokio::test]
    async fn admin_system_version_route_is_mounted_without_regressing_existing_routes()
    -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        std::fs::create_dir_all(dir.path().join("assets"))?;
        std::fs::write(dir.path().join("assets/app.js"), "console.log('conduit');")?;

        let app = build_router_with_static_root(AppState::default(), dir.path());
        let request = Request::builder()
            .uri("/api/system/version")
            .body(Body::empty())?;

        let response = app.clone().call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 4096).await?;
        let body = serde_json::from_slice::<Value>(&body)?;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("application/json"));
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

        let response = app
            .clone()
            .call(Request::builder().uri("/health").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // LLM routes now sit behind the API-key auth layer (Go apiGroup uses
        // `middleware.WithAPIKeyConfig`, routes.go:167). A bearer key is
        // required to reach the handler; the extraction middleware only checks
        // presence, so any non-empty key passes through to `list_models`.
        let response = app
            .clone()
            .call(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, "Bearer test-key")
                    .body(Body::empty())?,
            )
            .await?;
        // Without a ModelService wired, list_models returns 500 (not 501
        // anymore — the handler is now a real dispatch, not a placeholder).
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let response = app
            .clone()
            .call(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        Ok(())
    }

    #[tokio::test]
    async fn openai_models_route_without_service_returns_internal_error()
    -> Result<(), Box<dyn Error>> {
        // /v1/models now dispatches through the real list_models handler. With
        // no ModelService wired (bare AppState::default()), it degrades to the
        // same internal-error branch Go hits when the model service is
        // unavailable.
        let mut app = build_router(state_with_validator());
        // `/v1/*` sits behind the API-key auth layer (Go apiGroup,
        // routes.go:167). Supply a bearer key so the request reaches the
        // handler instead of short-circuiting at the 401 extraction guard.
        let request = Request::builder()
            .uri("/v1/models")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 4096).await?;
        let body = serde_json::from_slice::<Value>(&body)?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(content_type.starts_with("application/json"));
        assert_eq!(body["error"]["type"], "internal_error");

        Ok(())
    }

    #[tokio::test]
    async fn anthropic_model_route_requires_wired_model_service() -> Result<(), Box<dyn Error>> {
        let mut app = build_router(state_with_validator());
        // `/anthropic/*` sits behind the API-key auth layer (Go apiGroup,
        // routes.go:167). Supply a bearer key so the request reaches the
        // handler instead of short-circuiting at the 401 extraction guard.
        let request = Request::builder()
            .uri("/anthropic/v1/models")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 4096).await?;
        let body = serde_json::from_slice::<Value>(&body)?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(content_type.starts_with("application/json"));
        assert_eq!(body["error"]["type"], "internal_server_error");

        Ok(())
    }

    #[tokio::test]
    async fn gemini_model_routes_require_wired_model_service() -> Result<(), Box<dyn Error>> {
        for path in ["/v1beta/models", "/gemini/v1/models"] {
            let mut app = build_router(state_with_validator());
            // `/v1beta/*` and `/gemini/*` sit behind the API-key auth layer
            // (Go apiGroup, routes.go:167). Supply a bearer key so the request
            // reaches the handler instead of the 401 extraction guard.
            let request = Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer test-key")
                .body(Body::empty())?;

            let response = app.call(request).await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(response.into_body(), 4096).await?;
            let body = serde_json::from_slice::<Value>(&body)?;

            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
            assert!(content_type.starts_with("application/json"), "{path}");
            assert_eq!(body["error"]["status"], "internal_server_error", "{path}");
        }

        Ok(())
    }

    #[tokio::test]
    async fn anthropic_count_tokens_routes_dispatch_to_orchestrator() -> Result<(), Box<dyn Error>>
    {
        // With no orchestrator injected, both aliases reach the live dispatcher
        // and fail in the standard Anthropic internal-error envelope.
        for path in [
            "/v1/messages/count_tokens",
            "/anthropic/v1/messages/count_tokens",
        ] {
            let mut app = build_router(state_with_validator());
            // `/v1/messages*` and `/anthropic/*` sit behind the API-key auth
            // layer (Go apiGroup, routes.go:167). Supply a bearer key so the
            // request reaches the handler instead of the 401 extraction guard.
            let request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "messages-2023-12-15")
                .header(header::AUTHORIZATION, "Bearer test-key")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))?;

            let response = app.call(request).await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(response.into_body(), 4096).await?;
            let body = serde_json::from_slice::<Value>(&body)?;

            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
            assert!(content_type.starts_with("application/json"), "{path}");
            assert_eq!(body["type"], "api_error", "{path}");
            assert_eq!(body["error"]["type"], "api_error", "{path}");
            assert_eq!(body["error"]["message"], "Internal server error", "{path}");
        }

        Ok(())
    }

    #[tokio::test]
    async fn openai_routes_reject_wrong_method_without_spa_fallback() -> Result<(), Box<dyn Error>>
    {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        let mut app = build_router_with_static_root(state_with_validator(), dir.path());
        // `/v1/*` sits behind the API-key auth layer (Go apiGroup,
        // routes.go:167), whose `route_layer` runs before axum's per-method
        // dispatch. Supply a bearer key + a wired validator so the request
        // passes auth and reaches the method check — otherwise a wrong-method
        // request short-circuits (401 no key / 500 no validator) instead of the
        // 405 we're asserting.
        let request = Request::builder()
            .method(Method::GET)
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, "Bearer test-key")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert!(!body.contains("<html>conduit</html>"));

        Ok(())
    }

    #[tokio::test]
    async fn frontend_fallback_returns_index_html() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        let mut app = build_router_with_static_root(AppState::default(), dir.path());
        let request = Request::builder()
            .uri("/dashboard/projects/1")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        // Go-parity: serveSPAIndex sets the full no-cache directive.
        assert_eq!(cache_control, crate::static_files::SPA_INDEX_CACHE_CONTROL);
        assert!(body.contains("<base href=\"/\">"), "{body}");
        assert!(
            body.contains("<meta name=\"conduit-base-path\" content=\"\">"),
            "{body}"
        );
        assert!(body.ends_with("<html>conduit</html>"), "{body}");

        Ok(())
    }

    #[tokio::test]
    async fn api_fallback_returns_json_not_index_html() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;

        for path in ["/v1/nonexistent-endpoint", "/admin/nonexistent-endpoint"] {
            let mut app = build_router_with_static_root(AppState::default(), dir.path());
            // API paths sit behind auth layers; provide a bearer key so the
            // request reaches the fallback instead of being blocked at 401.
            let request = Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer test-key")
                .body(Body::empty())?;

            let response = app.call(request).await?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let body = to_bytes(response.into_body(), 1024).await?;
            let body = std::str::from_utf8(&body)?;

            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(content_type.starts_with("application/json"));
            assert!(body.contains("\"code\":\"not_found\""));
            assert!(!body.contains("<html>conduit</html>"));
        }

        Ok(())
    }

    #[tokio::test]
    async fn static_asset_returns_file_content() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        std::fs::create_dir_all(dir.path().join("assets"))?;
        std::fs::write(dir.path().join("assets/app.js"), "console.log('conduit');")?;
        let mut app = build_router_with_static_root(AppState::default(), dir.path());
        let request = Request::builder()
            .uri("/assets/app.js")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/javascript"));
        assert!(cache_control.starts_with("public"));
        assert_eq!(body, "console.log('conduit');");

        Ok(())
    }

    #[tokio::test]
    async fn missing_asset_with_extension_returns_json_not_index() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        let mut app = build_router_with_static_root(AppState::default(), dir.path());
        let request = Request::builder()
            .uri("/assets/missing.css")
            .body(Body::empty())?;

        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(content_type.starts_with("application/json"));
        assert!(body.contains("\"code\":\"static_asset_not_found\""));
        assert!(!body.contains("<html>conduit</html>"));

        Ok(())
    }

    #[test]
    fn base_path_mount_decision_strips_only_matching_prefix() {
        assert_eq!(
            strip_base_path_for_mount("/gateway/v1/models", "/gateway"),
            Some("/v1/models")
        );
        assert_eq!(
            strip_base_path_for_mount("/gateway/dashboard", "/gateway"),
            Some("/dashboard")
        );
        assert_eq!(strip_base_path_for_mount("/gateway", "/gateway"), Some("/"));
        assert_eq!(strip_base_path_for_mount("/health", ""), Some("/health"));
        assert_eq!(
            strip_base_path_for_mount("/gatewayish/v1/models", "/gateway"),
            None
        );
    }

    #[test]
    fn base_path_mount_decision_preserves_api_and_spa_fallback_semantics() {
        let Some(api_path) = strip_base_path_for_mount("/gateway/v1/models", "/gateway") else {
            panic!("path should be inside base path");
        };
        let Some(spa_path) = strip_base_path_for_mount("/gateway/dashboard/projects/1", "/gateway")
        else {
            panic!("path should be inside base path");
        };

        assert_eq!(
            decide_static_fallback(api_path, "dist"),
            StaticFallbackDecision::ApiNotFound
        );
        assert_eq!(
            decide_static_fallback(spa_path, "dist"),
            StaticFallbackDecision::FrontendIndex {
                index_path: PathBuf::from("dist").join("index.html")
            }
        );
    }

    #[test]
    fn validate_base_path_accepts_empty_root_and_simple_paths() -> Result<(), String> {
        for ok in ["", "/", "/api", "/gateway/v1", "/a-b_c.d~0"] {
            validate_base_path(ok)?;
        }
        Ok(())
    }

    #[test]
    fn validate_base_path_rejects_missing_leading_slash() {
        let err = match validate_base_path("api") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of base path without leading '/'"),
        };
        assert!(err.contains("start"), "{err}");
    }

    #[test]
    fn validate_base_path_rejects_trailing_slash_except_root() {
        let err = match validate_base_path("/api/") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of trailing '/'"),
        };
        assert!(err.contains("end"), "{err}");

        // Root "/" is the single permitted trailing-slash value.
        assert!(validate_base_path("/").is_ok());
    }

    #[test]
    fn validate_base_path_rejects_query_fragment_and_unsafe_chars() {
        let err = match validate_base_path("/api?x=1") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of query"),
        };
        assert!(err.contains("query or fragment"), "{err}");

        let err = match validate_base_path("/api#frag") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of fragment"),
        };
        assert!(err.contains("query or fragment"), "{err}");

        let err = match validate_base_path("/api path") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of unsafe space"),
        };
        assert!(err.contains("URL-safe"), "{err}");
    }

    #[test]
    fn mount_path_passes_route_through_when_base_is_empty_or_root() {
        assert_eq!(mount_path("", "/v1/models"), "/v1/models");
        assert_eq!(mount_path("/", "/v1/models"), "/v1/models");
        assert_eq!(mount_path("", "/health"), "/health");
    }

    #[test]
    fn mount_path_joins_base_and_route_without_doubling_slash() {
        assert_eq!(mount_path("/gateway", "/v1/models"), "/gateway/v1/models");
        assert_eq!(mount_path("/gateway/", "/v1/models"), "/gateway/v1/models");
        assert_eq!(mount_path("/gateway", "/"), "/gateway");
        assert_eq!(
            mount_path("/gateway", "/admin/system/status"),
            "/gateway/admin/system/status"
        );
    }

    #[test]
    fn mount_path_is_idempotent_when_route_already_prefixed() {
        assert_eq!(
            mount_path("/gateway", "/gateway/v1/models"),
            "/gateway/v1/models"
        );
        assert_eq!(mount_path("/v1", "/v1beta/models"), "/v1/v1beta/models");
    }

    #[test]
    fn mount_path_handles_nested_base_segment() {
        assert_eq!(mount_path("/a/b", "/v1/models"), "/a/b/v1/models");
    }

    #[test]
    fn should_expose_root_health_gates_on_non_empty_base_path() {
        // Empty base path: /health is always exposed at root (matches Go routes.go:79).
        assert!(should_expose_root_health("", false));
        assert!(should_expose_root_health("", true));

        // Non-empty base path: root /health is suppressed unless compat flag opts in.
        assert!(!should_expose_root_health("/gateway", false));
        assert!(should_expose_root_health("/gateway", true));

        // Trailing slash on base path is normalized away before the decision.
        assert!(!should_expose_root_health("/gateway/", false));
        assert!(should_expose_root_health("/gateway/", true));

        // Root "/" as base path counts as "empty" for this decision.
        assert!(should_expose_root_health("/", false));
    }

    #[test]
    fn base_path_lifecycle_end_to_end_validate_then_mount_then_health_gate() -> Result<(), String> {
        // Simulate S11/S16 lifecycle for a non-empty base path.
        let base_path = "/gateway";
        validate_base_path(base_path)?;

        // API routes mount under the base path.
        assert_eq!(mount_path(base_path, "/v1/models"), "/gateway/v1/models");
        assert_eq!(
            mount_path(base_path, "/anthropic/v1/messages"),
            "/gateway/anthropic/v1/messages"
        );

        // Static fallback also mounts under the base path (S16).
        assert_eq!(mount_path(base_path, "/"), "/gateway");

        // Root /health is gated off unless compat flag is set.
        assert!(!should_expose_root_health(base_path, false));
        assert!(should_expose_root_health(base_path, true));

        Ok(())
    }

    #[tokio::test]
    async fn serve_listener_stops_after_shutdown_signal() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let server = tokio::spawn(serve_listener(listener, AppState::default(), async {
            let _ = shutdown_rx.await;
        }));

        let _ = shutdown_tx.send(());
        timeout(Duration::from_secs(2), server).await???;

        Ok(())
    }

    #[tokio::test]
    async fn graceful_shutdown_timeout_bounds_a_stalled_connection() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(serve_listener_with_graceful_timeout(
            listener,
            AppState::default(),
            async {
                let _ = shutdown_rx.await;
            },
            Duration::from_millis(20),
        ));

        // An incomplete request keeps the accepted HTTP connection active.
        // The configured drain deadline must still terminate the server.
        let mut client = TcpStream::connect(address).await?;
        client
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
            .await?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = shutdown_tx.send(());

        let joined = timeout(Duration::from_secs(2), server).await?;
        let result = joined?;
        match result {
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::TimedOut),
            Ok(()) => panic!("stalled connection must exceed the drain deadline"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_helper_can_be_driven_by_test_interrupt() -> Result<(), Box<dyn Error>> {
        let (interrupt_tx, interrupt_rx) = oneshot::channel::<()>();
        let (_terminate_tx, terminate_rx) = oneshot::channel::<()>();

        let _ = interrupt_tx.send(());
        let signal = shutdown_signal_from(
            async {
                let _ = interrupt_rx.await;
            },
            async {
                let _ = terminate_rx.await;
            },
        )
        .await;

        assert_eq!(signal, ShutdownSignalKind::Interrupt);

        Ok(())
    }

    #[tokio::test]
    async fn shutdown_helper_can_be_driven_by_test_terminate() -> Result<(), Box<dyn Error>> {
        let (_interrupt_tx, interrupt_rx) = oneshot::channel::<()>();
        let (terminate_tx, terminate_rx) = oneshot::channel::<()>();

        let _ = terminate_tx.send(());
        let signal = shutdown_signal_from(
            async {
                let _ = interrupt_rx.await;
            },
            async {
                let _ = terminate_rx.await;
            },
        )
        .await;

        assert_eq!(signal, ShutdownSignalKind::Terminate);

        Ok(())
    }

    // ---- RUST-P2-001 S07/S10/S13 staged init + shutdown plan tests ----

    /// S08: the boot sequence is a fixed, ordered list matching
    /// `config -> logging -> db -> cache -> services -> scheduler ->
    /// router -> server`. This mirrors the implicit order enforced by
    /// `go.uber.org/fx` in `conduit/internal/server/server.go` `Run()`
    /// (constructors + Module wiring) and `cmd/conduit/main.go`
    /// `startServer` (lifecycle hooks). We make it explicit so the Rust
    /// binary walks stages deterministically.
    #[test]
    fn init_sequence_stages_match_s08_contract() -> Result<(), String> {
        assert_eq!(
            InitSequence::STAGES,
            [
                HttpInitStage::Config,
                HttpInitStage::Logging,
                HttpInitStage::Db,
                HttpInitStage::Cache,
                HttpInitStage::Services,
                HttpInitStage::Scheduler,
                HttpInitStage::Router,
                HttpInitStage::Server,
            ]
        );
        Ok(())
    }

    /// S08: `next_stage` advances the cursor one stage at a time,
    /// returns `None` after the final stage, and never wraps.
    #[test]
    fn init_sequence_next_stage_advances_monotonically() -> Result<(), String> {
        let mut seq = InitSequence::new();

        assert_eq!(seq.current(), Some(HttpInitStage::Config));
        assert_eq!(seq.completed(), &[] as &[HttpInitStage]);
        assert!(!seq.is_done());

        assert_eq!(seq.next_stage(), Some(HttpInitStage::Logging));
        assert_eq!(seq.completed(), &[HttpInitStage::Config]);

        assert_eq!(seq.next_stage(), Some(HttpInitStage::Db));
        assert_eq!(seq.next_stage(), Some(HttpInitStage::Cache));
        assert_eq!(seq.next_stage(), Some(HttpInitStage::Services));
        assert_eq!(seq.next_stage(), Some(HttpInitStage::Scheduler));
        assert_eq!(seq.next_stage(), Some(HttpInitStage::Router));
        assert_eq!(seq.next_stage(), Some(HttpInitStage::Server));
        assert_eq!(seq.current(), Some(HttpInitStage::Server));
        assert_eq!(
            seq.completed(),
            &InitSequence::STAGES[..7] as &[HttpInitStage]
        );

        // Past the last stage: cursor does not advance, returns None.
        assert_eq!(seq.next_stage(), None);
        assert_eq!(seq.next_stage(), None);
        assert!(seq.is_done());

        Ok(())
    }

    /// S08: `reset` returns the cursor to before the first stage.
    #[test]
    fn init_sequence_reset_returns_to_start() -> Result<(), String> {
        let mut seq = InitSequence::new();
        let _ = seq.next_stage();
        let _ = seq.next_stage();
        assert_eq!(seq.current(), Some(HttpInitStage::Db));

        seq.reset();
        assert_eq!(seq.current(), Some(HttpInitStage::Config));
        assert_eq!(seq.completed(), &[] as &[HttpInitStage]);

        Ok(())
    }

    /// S13: `stage_error` records the failing stage, the cause message,
    /// and the slice of already-completed stages for ops triage.
    #[test]
    fn stage_error_records_stage_and_completed_stages() -> Result<(), String> {
        let err = stage_error(HttpInitStage::Db, "connection refused");

        assert_eq!(err.stage, HttpInitStage::Db);
        assert_eq!(err.message, "connection refused");
        // Db is the 3rd stage in S08 order; Config + Logging precede it.
        assert_eq!(
            err.completed_stages,
            [HttpInitStage::Config, HttpInitStage::Logging]
        );

        Ok(())
    }

    /// S13: a failure at the very first stage reports zero completed
    /// stages, and the triage line renders "after: none".
    #[test]
    fn stage_error_at_first_stage_has_no_prior_stages() -> Result<(), String> {
        let err = stage_error(HttpInitStage::Config, "missing config.yml");

        assert_eq!(err.completed_stages, &[] as &[HttpInitStage]);
        assert_eq!(
            err.triage_line(),
            "config initialization failed: missing config.yml (after: none)"
        );
        assert_eq!(
            err.to_string(),
            "config initialization failed: missing config.yml (after: none)"
        );

        Ok(())
    }

    /// S13: the triage line lists all prior stages comma-separated so an
    /// operator can see exactly how far boot got.
    #[test]
    fn stage_error_triage_line_lists_completed_stages() -> Result<(), String> {
        let err = stage_error(HttpInitStage::Router, "no base route mounted");

        assert_eq!(
            err.triage_line(),
            "router initialization failed: no base route mounted \
             (after: config, logging, db, cache, services, scheduler)"
        );

        Ok(())
    }

    /// S13: a stage that is not on the S08 contract path (e.g.
    /// `Listener`, which is a sub-step of `Server` in this model) reports
    /// zero completed stages rather than panicking.
    #[test]
    fn stage_error_for_off_contract_stage_is_safe() -> Result<(), String> {
        let err = stage_error(HttpInitStage::Listener, "bind failed");

        assert_eq!(err.stage, HttpInitStage::Listener);
        assert_eq!(err.completed_stages, &[] as &[HttpInitStage]);

        Ok(())
    }

    /// S13: every stage renders the contract as_str name.
    #[test]
    fn http_init_stage_as_str_covers_all_variants() -> Result<(), String> {
        assert_eq!(HttpInitStage::Config.as_str(), "config");
        assert_eq!(HttpInitStage::Logging.as_str(), "logging");
        assert_eq!(HttpInitStage::Db.as_str(), "db");
        assert_eq!(HttpInitStage::Cache.as_str(), "cache");
        assert_eq!(HttpInitStage::Services.as_str(), "services");
        assert_eq!(HttpInitStage::Scheduler.as_str(), "scheduler");
        assert_eq!(HttpInitStage::Router.as_str(), "router");
        assert_eq!(HttpInitStage::Listener.as_str(), "listener");
        assert_eq!(HttpInitStage::Server.as_str(), "server");

        Ok(())
    }

    /// S07/S10: the default shutdown plan matches the Go fx defaults —
    /// 30s drain timeout (fx.StopTimeout(30*time.Second) in main.go:62)
    /// and no explicit in-flight cap (Go stdlib Shutdown does not bound
    /// in-flight work).
    #[test]
    fn shutdown_plan_default_matches_go_fx_defaults() -> Result<(), String> {
        let plan = ShutdownPlan::default();
        assert_eq!(plan.drain_timeout, Duration::from_secs(30));
        assert_eq!(plan.in_flight_bound, usize::MAX);
        assert!(!plan.has_in_flight_cap());
        assert_eq!(ShutdownPlan::DEFAULT, plan);

        Ok(())
    }

    /// S07/S10: builders produce derived plans without mutating the
    /// original.
    #[test]
    fn shutdown_plan_builders_are_chainable_and_pure() -> Result<(), String> {
        let base = ShutdownPlan::default();
        let derived = base
            .with_drain_timeout(Duration::from_secs(5))
            .with_in_flight_bound(128);

        // Original is untouched.
        assert_eq!(base.drain_timeout, Duration::from_secs(30));
        assert!(!base.has_in_flight_cap());

        // Derived carries both overrides.
        assert_eq!(derived.drain_timeout, Duration::from_secs(5));
        assert_eq!(derived.in_flight_bound, 128);
        assert!(derived.has_in_flight_cap());

        Ok(())
    }

    /// S07/S10: `select_shutdown_signal` resolves whichever candidate
    /// fires first (interrupt path) and attaches the supplied plan,
    /// encoding the tokio::select-or behavior.
    #[tokio::test]
    async fn select_shutdown_signal_interrupt_wins_and_carries_plan() -> Result<(), Box<dyn Error>>
    {
        let (interrupt_tx, interrupt_rx) = oneshot::channel::<()>();
        let (_terminate_tx, terminate_rx) = oneshot::channel::<()>();

        let plan = ShutdownPlan::default().with_in_flight_bound(64);
        let _ = interrupt_tx.send(());

        let signal = select_shutdown_signal(
            async {
                let _ = interrupt_rx.await;
            },
            async {
                let _ = terminate_rx.await;
            },
            plan,
        )
        .await;

        assert_eq!(signal.kind, ShutdownSignalKind::Interrupt);
        assert_eq!(signal.plan.in_flight_bound, 64);
        assert_eq!(signal.plan.drain_timeout, Duration::from_secs(30));

        Ok(())
    }

    /// S07/S10: `select_shutdown_signal` resolves the terminate path
    /// symmetrically when only the terminate future fires.
    #[tokio::test]
    async fn select_shutdown_signal_terminate_wins_and_carries_plan() -> Result<(), Box<dyn Error>>
    {
        let (_interrupt_tx, interrupt_rx) = oneshot::channel::<()>();
        let (terminate_tx, terminate_rx) = oneshot::channel::<()>();

        let plan = ShutdownPlan::default().with_drain_timeout(Duration::from_secs(10));
        let _ = terminate_tx.send(());

        let signal = select_shutdown_signal(
            async {
                let _ = interrupt_rx.await;
            },
            async {
                let _ = terminate_rx.await;
            },
            plan,
        )
        .await;

        assert_eq!(signal.kind, ShutdownSignalKind::Terminate);
        assert_eq!(signal.plan.drain_timeout, Duration::from_secs(10));

        Ok(())
    }

    // ---- P1-003 S11/S16: base_path mounting integration ----

    /// Build a router whose `server.base_path` is `base_path` and a static
    /// root containing a single `index.html` (so the SPA fallback is live).
    fn router_with_base_path(base_path: &str) -> Result<(Router, AppConfig), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("assets"))?;
        std::fs::write(
            dir.path().join("index.html"),
            "<html><head></head><body>conduit</body></html>",
        )?;
        std::fs::write(dir.path().join("assets/app.js"), "console.log('ok')")?;
        let static_root = dir.path().to_path_buf();
        // Leak the tempdir for the test duration (the test process exits soon).
        std::mem::forget(dir);

        let mut config = AppConfig::default();
        config.server.base_path = base_path.to_string();
        let state = AppState::from_config(config.clone());
        let router = build_router_with_static_root(state, static_root);
        Ok((router, config))
    }

    #[tokio::test]
    async fn base_path_mounts_api_routes_under_prefix() -> Result<(), Box<dyn Error>> {
        let (mut app, _config) = router_with_base_path("/gateway")?;

        // /api/system/version is reachable only under /gateway/.
        let response = app
            .call(
                Request::builder()
                    .uri("/gateway/api/system/version")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // LLM routes also mount under the base path. They sit behind the
        // API-key auth layer (Go apiGroup, routes.go:167), so a bearer key is
        // required to reach the handler. Without a ModelService wired,
        // list_models returns 500 (real handler, no longer 501).
        let response = app
            .call(
                Request::builder()
                    .uri("/gateway/v1/models")
                    .header(header::AUTHORIZATION, "Bearer test-key")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        Ok(())
    }

    #[tokio::test]
    async fn base_path_suppresses_root_health_when_set() -> Result<(), Box<dyn Error>> {
        let (mut app, _config) = router_with_base_path("/gateway")?;

        let response = app
            .call(
                Request::builder()
                    .uri("/gateway/health")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // Root-level /health is NOT registered when base_path is non-empty.
        // It must NOT return the health JSON — instead it falls through to the
        // static fallback, which classifies `/health` as an API path and
        // returns a JSON 404 (proving the health handler did not run).
        let response = app
            .call(Request::builder().uri("/health").body(Body::empty())?)
            .await?;
        assert_ne!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024).await?;
        let body = std::str::from_utf8(&body)?;
        assert!(
            !body.contains("\"status\":\"ok\""),
            "root /health must not return the health JSON when base_path is set"
        );

        Ok(())
    }

    #[tokio::test]
    async fn base_path_mounts_spa_and_assets_without_leaking_root_fallback()
    -> Result<(), Box<dyn Error>> {
        let (mut app, _config) = router_with_base_path("/gateway")?;

        let response = app
            .clone()
            .call(
                Request::builder()
                    .uri("/gateway/assets/app.js")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .call(
                Request::builder()
                    .uri("/gateway/projects/1")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await?;
        let html = std::str::from_utf8(&body)?;
        assert!(html.contains("<base href=\"/gateway/\">"));
        assert!(html.contains("content=\"/gateway\""));

        let response = app
            .call(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn base_path_empty_keeps_root_health_and_root_routes() -> Result<(), Box<dyn Error>> {
        // Sanity: empty base_path must keep the legacy root-level routes.
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("index.html"), "<html>conduit</html>")?;
        let mut app = build_router_with_static_root(AppState::default(), dir.path());

        let response = app
            .clone()
            .call(Request::builder().uri("/health").body(Body::empty())?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);

        // `/v1/*` sits behind the API-key auth layer (Go apiGroup,
        // routes.go:167); supply a bearer key so the request reaches the
        // handler instead of stopping at the 401 extraction guard.
        let response = app
            .call(
                Request::builder()
                    .uri("/v1/models")
                    .header(header::AUTHORIZATION, "Bearer test-key")
                    .body(Body::empty())?,
            )
            .await?;
        // Real list_models handler returns 500 without a ModelService wired.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        Ok(())
    }

    #[tokio::test]
    async fn base_path_routes_not_reachable_at_unprefixed_root() -> Result<(), Box<dyn Error>> {
        let (mut app, _config) = router_with_base_path("/gateway")?;

        // /v1/models at root must NOT match the LLM API route; with base_path
        // set it must not return 500 (the real handler's no-service status). It
        // falls through to the static fallback, which classifies /v1/* as an
        // API path and returns a JSON 404 — proving the API route was not hit.
        let response = app
            .call(Request::builder().uri("/v1/models").body(Body::empty())?)
            .await?;
        assert_ne!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "root /v1/models must not hit the API handler when base_path is set"
        );

        Ok(())
    }
}

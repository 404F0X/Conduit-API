use std::sync::Arc;
use std::time::Duration;

use conduit_config::AppConfig;

use crate::auth_handlers::{SigninService, SignupService};
use crate::middleware::api_key_auth::ApiKeyValidationService;
use crate::middleware::metrics::MetricsState;
pub use crate::middleware::{JwtIdentityResolution, JwtIdentityResolver, JwtUserIdentity};
use crate::oauth_handlers::OAuthAdminService;
use crate::oidc_handlers::OidcService;
use crate::openai_handlers::{ModelService, OpenAiOrchestratorService, VideoService};
use crate::request_content_handlers::RequestContentService;
use crate::request_preview_handlers::RequestPreviewService;
use crate::system_handlers::SystemService;

/// Service bag injected into [`AppState`] (RUST-P11-003 S10).
///
/// Mirrors the Go fx wiring where handlers receive concrete `*biz.*Service`
/// values (`api/system.go:22-36`, `api/auth.go:14-28`): the Rust handlers
/// depend only on the minimal traits defined next to them; the host binary
/// wires concrete implementations (conduit-services) here at boot. Fields are
/// `Option` so a bare `AppState::default()` still builds a router — handlers
/// degrade to the Go 5xx error branches when a service is absent.
#[derive(Default)]
pub struct AppServices {
    system: Option<Arc<dyn SystemService>>,
    signin: Option<Arc<dyn SigninService>>,
    signup: Option<Arc<dyn SignupService>>,
    oidc: Option<Arc<dyn OidcService>>,
    request_content: Option<Arc<dyn RequestContentService>>,
    request_preview: Option<Arc<dyn RequestPreviewService>>,
    openai_orchestrator: Option<Arc<dyn OpenAiOrchestratorService>>,
    model: Option<Arc<dyn ModelService>>,
    video: Option<Arc<dyn VideoService>>,
    oauth_admin: Option<Arc<dyn OAuthAdminService>>,
    admin_schema: Option<conduit_admin_graphql::AdminSchema>,
    openapi_schema: Option<conduit_openapi_graphql::OpenApiSchema>,
    api_key_validation: Option<Arc<dyn ApiKeyValidationService>>,
    user_principal: Option<Arc<dyn JwtIdentityResolver>>,
}

impl AppServices {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wire the system service backing `/admin/system/status` + `/initialize`.
    pub fn with_system_service(mut self, service: Arc<dyn SystemService>) -> Self {
        self.system = Some(service);
        self
    }

    /// Wire the signin service backing `/admin/auth/signin`.
    pub fn with_signin_service(mut self, service: Arc<dyn SigninService>) -> Self {
        self.signin = Some(service);
        self
    }

    /// Wire the public email/password registration service.
    pub fn with_signup_service(mut self, service: Arc<dyn SignupService>) -> Self {
        self.signup = Some(service);
        self
    }

    /// Wire the OIDC service backing `/oauth/oidc/*` + `/admin/oidc/link/*`
    /// (RUST-P11-003 S03; Go fx wires `*biz.OIDCService` + `*biz.AuthService`
    /// into `api.OIDCHandlers`, oidc.go:25-43).
    pub fn with_oidc_service(mut self, service: Arc<dyn OidcService>) -> Self {
        self.oidc = Some(service);
        self
    }

    pub fn system_service(&self) -> Option<&Arc<dyn SystemService>> {
        self.system.as_ref()
    }

    pub fn signin_service(&self) -> Option<&Arc<dyn SigninService>> {
        self.signin.as_ref()
    }

    pub fn signup_service(&self) -> Option<&Arc<dyn SignupService>> {
        self.signup.as_ref()
    }

    pub fn oidc_service(&self) -> Option<&Arc<dyn OidcService>> {
        self.oidc.as_ref()
    }

    /// Wire the request-content service backing
    /// `/admin/requests/{request_id}/content` (RUST-P11-001 MAP-02; Go fx
    /// wires `*biz.DataStorageService` into `api.RequestContentHandlers`,
    /// request_content.go:20-34).
    pub fn with_request_content_service(mut self, service: Arc<dyn RequestContentService>) -> Self {
        self.request_content = Some(service);
        self
    }

    /// Wire the request-preview service backing
    /// `/admin/requests/{request_id}/preview` (RUST-P11-001 MAP-02; Go fx
    /// wires `*biz.RequestService` + `*biz.LiveStreamRegistry` into
    /// `api.RequestPreviewHandlers`, request_live.go:27-38).
    pub fn with_request_preview_service(mut self, service: Arc<dyn RequestPreviewService>) -> Self {
        self.request_preview = Some(service);
        self
    }

    pub fn request_content_service(&self) -> Option<&Arc<dyn RequestContentService>> {
        self.request_content.as_ref()
    }

    pub fn request_preview_service(&self) -> Option<&Arc<dyn RequestPreviewService>> {
        self.request_preview.as_ref()
    }

    /// Wire the OpenAI orchestrator service backing `/v1/chat/completions`,
    /// `/v1/responses`, and `/v1/embeddings` (RUST-P11-001 MAP-01; Go fx
    /// wires `*orchestrator.ChatCompletionOrchestrator` into
    /// `api.OpenAIHandlers`, openai.go:73-289). The host bridges it to the
    /// `conduit-orchestrator::CommandOrchestrator` plus the per-route
    /// inbound transformer selection at boot.
    pub fn with_openai_orchestrator(mut self, service: Arc<dyn OpenAiOrchestratorService>) -> Self {
        self.openai_orchestrator = Some(service);
        self
    }

    pub fn openai_orchestrator_service(&self) -> Option<&Arc<dyn OpenAiOrchestratorService>> {
        self.openai_orchestrator.as_ref()
    }

    /// Wire the model service backing `/v1/models` (list) and
    /// `/v1/models/{model}` (retrieve). Mirrors Go fx wiring of
    /// `*biz.ModelService` into `api.OpenAIHandlers` (openai.go:38-67).
    pub fn with_model_service(mut self, service: Arc<dyn ModelService>) -> Self {
        self.model = Some(service);
        self
    }

    pub fn model_service(&self) -> Option<&Arc<dyn ModelService>> {
        self.model.as_ref()
    }

    /// Wire the video service backing `/v1/videos` (create/get/delete). Mirrors
    /// Go fx wiring of `*biz.VideoService` into `api.OpenAIHandlers` plus the
    /// `VideoInboundTransformer` (openai.go:73-94, 384-468). The host bridges
    /// this to the P7-006 S08/S12 `conduit-services::video_service::VideoTaskService`.
    pub fn with_video_service(mut self, service: Arc<dyn VideoService>) -> Self {
        self.video = Some(service);
        self
    }

    pub fn video_service(&self) -> Option<&Arc<dyn VideoService>> {
        self.video.as_ref()
    }

    /// Wire the OAuth-admin service backing `/admin/{provider}/oauth/*` +
    /// `/admin/codex/auth/decode` (RUST-P11-003 S08; Go fx wires
    /// `api.CodexHandlers` + `api.ClaudeCodeHandlers` from
    /// `xcache.CacheConfig` + `*httpclient.HttpClient`, codex.go:24-41 +
    /// claudecode.go:24-41). The host bridges it to a concrete
    /// `OAuthAdminService` (TTL cache + HTTP client token-exchange wiring
    /// live behind the trait).
    pub fn with_oauth_admin_service(mut self, service: Arc<dyn OAuthAdminService>) -> Self {
        self.oauth_admin = Some(service);
        self
    }

    pub fn oauth_admin_service(&self) -> Option<&Arc<dyn OAuthAdminService>> {
        self.oauth_admin.as_ref()
    }

    pub fn with_admin_schema(mut self, schema: conduit_admin_graphql::AdminSchema) -> Self {
        self.admin_schema = Some(schema);
        self
    }

    pub fn admin_schema(&self) -> Option<&conduit_admin_graphql::AdminSchema> {
        self.admin_schema.as_ref()
    }

    pub fn with_openapi_schema(mut self, schema: conduit_openapi_graphql::OpenApiSchema) -> Self {
        self.openapi_schema = Some(schema);
        self
    }

    pub fn openapi_schema(&self) -> Option<&conduit_openapi_graphql::OpenApiSchema> {
        self.openapi_schema.as_ref()
    }

    pub fn with_api_key_validation_service(
        mut self,
        service: Arc<dyn ApiKeyValidationService>,
    ) -> Self {
        self.api_key_validation = Some(service);
        self
    }

    pub fn api_key_validation_service(&self) -> Option<&Arc<dyn ApiKeyValidationService>> {
        self.api_key_validation.as_ref()
    }

    /// Wire the JWT identity resolver — the user-load half of Go
    /// `AuthenticateJWTToken` (`biz/auth.go:192-201`). When wired, the JWT
    /// middleware rejects tokens whose user is missing/deactivated (401) and
    /// folds `is_owner` + scopes onto the principal. Absent ⇒ the middleware
    /// keeps the claims-only principal (legacy secret-only wiring).
    pub fn with_user_principal_service(mut self, service: Arc<dyn JwtIdentityResolver>) -> Self {
        self.user_principal = Some(service);
        self
    }

    pub fn user_principal_service(&self) -> Option<&Arc<dyn JwtIdentityResolver>> {
        self.user_principal.as_ref()
    }
}

// Manual Debug: trait objects carry no Debug bound; report wiring presence only.
impl std::fmt::Debug for AppServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppServices")
            .field("system", &self.system.is_some())
            .field("signin", &self.signin.is_some())
            .field("oidc", &self.oidc.is_some())
            .field("request_content", &self.request_content.is_some())
            .field("request_preview", &self.request_preview.is_some())
            .field("openai_orchestrator", &self.openai_orchestrator.is_some())
            .field("model", &self.model.is_some())
            .field("video", &self.video.is_some())
            .field("oauth_admin", &self.oauth_admin.is_some())
            .field("admin_schema", &self.admin_schema.is_some())
            .field("openapi_schema", &self.openapi_schema.is_some())
            .field("api_key_validation", &self.api_key_validation.is_some())
            .field("user_principal", &self.user_principal.is_some())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct AppState {
    config: Arc<AppConfig>,
    services: Arc<AppServices>,
    metrics: MetricsState,
}

impl AppState {
    pub fn new(config: Arc<AppConfig>, services: Arc<AppServices>) -> Self {
        let metrics = MetricsState::new(config.metrics.enabled);
        Self {
            config,
            services,
            metrics,
        }
    }

    pub fn from_config(config: AppConfig) -> Self {
        Self::new(Arc::new(config), Arc::new(AppServices::default()))
    }

    pub fn config(&self) -> &Arc<AppConfig> {
        &self.config
    }

    pub fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    pub fn metrics(&self) -> &MetricsState {
        &self.metrics
    }

    pub fn request_timeout(&self) -> Duration {
        self.config.server.request_timeout
    }

    pub fn llm_request_timeout(&self) -> Duration {
        self.config.server.llm_request_timeout
    }

    /// The configured `server.base_path` (P1-003 S11/S16). Empty by default;
    /// when non-empty, `build_router_with_static_root` mounts every API route
    /// and the static fallback under this prefix and suppresses the root
    /// `/health` route (unless the compat flag re-enables it). Go declares
    /// `BasePath` but never wires it into route mounting; the Rust side
    /// implements the S11/S16 compat strategy explicitly.
    pub fn base_path(&self) -> &str {
        &self.config.server.base_path
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::from_config(AppConfig::default())
    }
}

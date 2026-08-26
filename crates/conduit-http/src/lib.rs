#![forbid(unsafe_code)]

pub mod admin_handlers;
pub mod aisdk_handlers;
pub mod anthropic_handlers;
pub mod api_error;
pub mod app_state;
pub mod asset_source;
pub mod auth_handlers;
pub mod doubao_handlers;
#[path = "middleware/error.rs"]
pub mod error_middleware;
pub mod gemini_handlers;
pub mod graphql_handlers;
pub mod health;
pub mod jina_handlers;
pub mod middleware;
pub mod oauth_handlers;
pub mod oidc_handlers;
pub mod oidc_helpers;
pub mod openai_handlers;
pub mod openapi_graphql_handlers;
#[path = "middleware/panic_layer.rs"]
pub mod panic_layer;
pub mod request_content_handlers;
pub mod request_content_helpers;
pub mod request_preview_handlers;
pub mod router;
pub mod static_files;
pub mod system_handlers;
pub mod webhook_handlers;

pub use admin_handlers::{
    SignInResponse, SystemStatusResponse, SystemVersionResponse, build_signin_response,
    build_system_status_response, check_initialize_precondition, system_version,
};
pub use aisdk_handlers::{
    AI_SDK_DATA_STREAM_CONTENT_TYPE, AI_SDK_JSON_CONTENT_TYPE, AiSdkResponseContentType,
    is_aisdk_data_stream_content_type, select_aisdk_response_content_type,
};
pub use anthropic_handlers::{
    ANTHROPIC_BETA_HEADER, ANTHROPIC_INVALID_MODEL_STATUS, ANTHROPIC_VERSION_HEADER,
    AnthropicErrorBody, AnthropicErrorEnvelope, AnthropicErrorKind, AnthropicHeaderCompatibility,
    AnthropicModelListResponse, AnthropicModelObject, AnthropicModelSummary, AnthropicRouteBase,
    AnthropicRouteKind, AnthropicRouteParts, anthropic_error_classification,
    anthropic_error_response, anthropic_error_response_from_envelope,
    anthropic_header_compatibility, anthropic_model_list_response, build_anthropic_error_envelope,
    parse_anthropic_route_parts,
};
pub use api_error::{http_status_text, is_valid_email, json_error, min_chars};
pub use app_state::{AppServices, AppState};
pub use auth_handlers::{AuthenticatedUser, SignInRequest, SigninError, SigninService, sign_in};
pub use doubao_handlers::{
    DOUBAO_TASKS_PATH, DoubaoTaskAction, DoubaoTaskRoute, doubao_task_route_path,
    parse_doubao_task_route,
};
pub use error_middleware::{
    ErrorFallbackContext, ErrorResponseFormat, conduit_error_response, fallback_panic_error,
    fallback_rejection_error, internal_fallback_error,
};
pub use gemini_handlers::{
    GEMINI_JSON_STREAM_CONTENT_TYPE, GEMINI_SSE_STREAM_CONTENT_TYPE, GeminiAlt,
    GeminiModelListResponse, GeminiModelListRouteParts, GeminiModelObject, GeminiModelSummary,
    GeminiRouteBase, GeminiRouteParts, GeminiStreamFormat, gemini_model_list_response,
    parse_gemini_model_list_response, parse_gemini_model_list_route_parts,
    parse_gemini_route_parts, select_gemini_stream_format,
};
pub use health::{HealthResponse, health};
pub use jina_handlers::{
    JinaEndpointKind, JinaRequestPolicy, JinaRouteParseError, jina_endpoint_kind_for_path,
    jina_request_policy_for_path, openai_compatible_rerank_policy_for_path,
};
pub use middleware::{
    BodyRewindError, CorsConfig, CorsDecision, CorsRequestKind, HttpRequestContext, IpBlocklist,
    IpDecision, MiddlewareOrderRecorder, RawRequestHeaders, RecordedMiddleware,
    RequestContextExtension, RequestMiddlewareDecision, RequestPrincipal, RequestSource,
    ThreadContext, TraceContext, TraceThreadContext, body_collect_limit, body_collect_limit_bytes,
    decide_request_middleware, extract_request_context, insert_request_context, middleware_order,
    record_middleware_order, request_context_for_route, rewind_body, source_for_route,
};
pub use oauth_handlers::{
    AdminOAuthRoute, CopilotPollStatus, DecodeAuthJsonRequest, DecodeAuthJsonResponse,
    ExchangeOAuthRequest, ExchangeOAuthResponse, OAuthAdminService, OAuthProvider,
    OAuthServerError, PollCopilotOAuthRequest, PollCopilotOAuthResponse, ProxyConfig,
    StartAntigravityOAuthRequest, StartAntigravityOAuthResponse, StartCopilotOAuthRequest,
    StartCopilotOAuthResponse, StartOAuthRequest, StartOAuthResponse, decode_auth_json,
    parse_admin_oauth_route, poll_oauth, start_oauth,
};
pub use oidc_handlers::{
    ExchangeRequest, MISSING_PUBLIC_URL_WARNING, OidcExchangedUser, OidcService, ProviderInfo,
    callback, callback_with_provider, exchange, get_authorize_url, get_link_authorize_url,
    get_providers, query_escape, resolve_base_url, should_warn_missing_public_url,
};
pub use oidc_helpers::{
    OidcStateSet, PKCE_REJECTED_ERROR, STATE_REJECTED_ERROR, consume_pkce_verifier, consume_state,
    pkce_challenge,
};
pub use openai_handlers::{
    ModelService, OpenAiErrorBody, OpenAiErrorEnvelope, OpenAiHandlerOutput, OpenAiHandlerResponse,
    OpenAiOrchestratorService, OpenAiRoute, StreamEvent, VideoService, create_chat_completion,
    create_embedding, create_response, create_speech, list_models, retrieve_model,
};
pub use panic_layer::{PanicCatchFuture, PanicCatchLayer, PanicCatchService};
pub use request_content_handlers::{
    ContentDataStorage, ContentFile, ContentOpenError, ContentRequestRow, ContentStorageType,
    DownloadContentRequest, ProjectIdRejection, RequestContentService, download_request_content,
    parse_request_id_param, resolve_project_id,
};
pub use request_content_helpers::{
    BINARY_CONTENT_TYPE, CONTENT_CACHE_CONTROL, ContentDisposition, ContentMeta, ContentRange,
    build_content_response_meta, filename_from_key, safe_relative_path,
};
pub use request_preview_handlers::{
    DONE_STREAM_EVENT_DATA, InMemoryPreviewChunkBuffer, MAX_CHUNK_CAPACITY, PREVIEW_CHUNK_EVENT,
    PREVIEW_COMPLETED_EVENT, PREVIEW_COMPLETED_EVENT_DATA, PREVIEW_IDLE_TIMEOUT,
    PREVIEW_REPLAY_EVENT, PreviewBufferRead, PreviewChunkBuffer, PreviewRequestRow,
    PreviewSubscription, REQUEST_STATUS_PROCESSING, RequestDetailPreviewContract,
    RequestPreviewFallbackResponse, RequestPreviewService, SSE_CONTENT_TYPE,
    is_preview_terminal_chunk, preview_request, request_detail_sse_contract, sse_event_frame,
};
pub use router::{
    HttpInitError, HttpInitStage, InitSequence, RouteGroupComposition, RouteGroupKind,
    RouteGroupMetadata, RouteTimeoutKind, ShutdownPlan, ShutdownSignal, ShutdownSignalKind,
    StagedInitError, build_router, http_init_error, metrics_router, mount_path,
    route_group_composition_for_path, route_group_for_path, select_shutdown_signal, serve_listener,
    serve_listener_with_graceful_timeout, should_expose_root_health, shutdown_signal,
    shutdown_signal_from, stage_error, strip_base_path_for_mount, validate_base_path,
};
pub use static_files::{
    StaticFallbackDecision, decide_static_fallback, default_static_root, is_api_path,
    serve_from_asset_source, static_cache_control_for_path, static_content_type_for_path,
    static_fallback_response,
};
pub use system_handlers::{
    InitializeSystemParams, InitializeSystemRequest, InitializeSystemResponse, SystemService,
    get_favicon, get_system_status, initialize_system,
};
pub use webhook_handlers::{WebhookEchoResponse, webhook_echo, webhook_echo_response};

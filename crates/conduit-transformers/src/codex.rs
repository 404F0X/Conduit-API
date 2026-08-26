//! Codex (ChatGPT backend) OAuth outbound transformer (RUST-P11-003 S08).
//!
//! Port of Go `conduit/llm/transformer/openai/codex/`:
//!   * `constants.go` — Codex model registry, OAuth endpoints, client id,
//!     originator, image fallback model.
//!   * `headers.go`   — Codex session/turn-metadata headers, passthrough list,
//!     session-id extraction.
//!   * `token.go`     — Codex `auth.json` decode + OAuth token provider
//!     defaults. Codex uses the shared Go `llm/oauth` package with the
//!     **form-encoded** exchange strategy (`oauth.FormEncodedStrategy`,
//!     `llm/oauth/exchange_strategy.go:23-87`); the strategy's pure request
//!     builders are ported here, while the shared credential/refresh decision
//!     pieces are reused from [`crate::claudecode`] (which hosts the oauth
//!     port until a dedicated shared module lands).
//!   * `utils.go`     — ChatGPT account-id extraction from the access-token
//!     JWT + Codex CLI version sniffing.
//!   * `outbound.go`  — the outbound wrapper around the OpenAI Responses
//!     transformer: structured phase ([`prepare_codex_request`]), HTTP
//!     decoration phase ([`decorate_codex_http_request`]), and the combined
//!     [`build_codex_http_request`] mirroring
//!     `OutboundTransformer.TransformRequest`.
//!
//! Style follows the crate's pure-decision-function convention (see
//! `claudecode.rs`): no I/O here. Not ported (async/executor wiring, mirrors
//! the claudecode precedent):
//!   * `codexExecutor` / `CustomizeExecutor` / `Stop` (outbound.go:265-374) —
//!     stream-aggregating executor + Responses WebSocket executor cache.
//!   * `TransformResponse` / `TransformStream` / `AggregateStreamChunks`
//!     delegation (outbound.go:235-263) — pending the Responses
//!     response/stream module port.
//!   * chat→responses input conversion and the image tool request builder
//!     (Go `responses/outbound_convert.go` / `responses/image_request.go`) —
//!     [`build_codex_http_request`] handles `Responses` payloads natively and
//!     reports the pending conversion for other payload kinds.

use conduit_core::ConduitError;
use conduit_llm::constants::{ApiFormat, RequestType};
use conduit_llm::model::{HeaderMap, HttpRequest, LlmRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TransformerResult;
use crate::claudecode::{ExchangeParams, OAuthCredentials, OAuthUrls, TokenGetter};
use crate::openai_outbound::{
    DEFAULT_RESPONSES_PATH, build_openai_outbound_body, resolve_outbound_url_for_endpoint,
};

// ---------------------------------------------------------------------------
// Constants — Go `codex/constants.go:1-36`.
// ---------------------------------------------------------------------------

/// Static list of Codex-capable model IDs. Mirrors Go `DefaultModels()`
/// (constants.go:7-24) verbatim — the ChatGPT Codex backend has no stable
/// public `/models` endpoint, so a local registry powers "Fetch Models".
pub const DEFAULT_MODELS: [&str; 14] = [
    "gpt-5",
    "gpt-5-codex",
    "gpt-5-codex-mini",
    "gpt-5.1",
    "gpt-5.1-codex",
    "gpt-5.1-codex-mini",
    "gpt-5.1-codex-max",
    "gpt-5.2",
    "gpt-5.2-codex",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.5",
];

/// Go `DefaultModels()` (constants.go:7) — returns a fresh owned list.
pub fn default_models() -> Vec<String> {
    DEFAULT_MODELS.iter().map(|m| (*m).to_string()).collect()
}

/// Go `defaultImageMainModel` (constants.go:27) — the main model Codex image
/// requests are rewritten to (the original model rides the image tool).
pub const DEFAULT_IMAGE_MAIN_MODEL: &str = "gpt-5.4-mini";

/// Go `Conduit APIOriginator` (constants.go:29).
pub const CONDUIT_ORIGINATOR: &str = "conduit";
/// Go `AuthorizeURL` (constants.go:30).
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
/// Go `TokenURL` (constants.go:32).
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Go `ClientID` (constants.go:33).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Go `RedirectURI` (constants.go:34).
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// Go `Scopes` (constants.go:35).
pub const SCOPES: &str = "openid profile email offline_access";

/// The API format the Codex transformer speaks. Mirrors Go
/// `OutboundTransformer.APIFormat()` returning `llm.APIFormatOpenAIResponse`
/// (outbound.go:85-87).
pub const CODEX_API_FORMAT: ApiFormat = ApiFormat::OpenAiResponses;

/// Go `codexBaseURL` (outbound.go:26) — the canonical Codex backend base URL
/// (the trailing `#` is the "raw URL" marker used by the URL resolver, see
/// [`crate::openai_outbound::resolve_outbound_url_for_endpoint`]).
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex#";

/// Go `codexAPIURL` (outbound.go:27) — the resolved Codex Responses endpoint
/// that all non-image Codex requests are POSTed to.
pub const CODEX_API_URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Default User-Agent the Conduit API HTTP client injects when no inbound UA is
/// forwarded (Go `httpclient` default at `llm/httpclient/client.go:374`). The
/// Codex transformer deliberately drops the inbound UA unless the caller
/// supplies one (outbound.go:198), so this is the value observed on the wire.
pub const DEFAULT_USER_AGENT: &str = "conduit/1.0";

/// HTTP header carrying the ChatGPT account id extracted from the
/// access-token JWT (Go outbound.go:229 sets it via
/// `http.Header.Set("Chatgpt-Account-Id", ...)`).
pub const CHATGPT_ACCOUNT_ID_HEADER: &str = "Chatgpt-Account-Id";

/// Go `codex.Params` (outbound.go:50-54) minus the `TokenProvider` handle,
/// which is passed separately as a [`TokenGetter`] trait object (Go requires
/// it non-nil at construction; the Rust builder takes it as a required
/// argument instead).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexParams {
    /// Channel-configured base URL. Go outbound.go:62-65: empty or
    /// `https://api.openai.com/v1` falls back to [`CODEX_BASE_URL`].
    pub base_url: String,
    /// Transport selection (Go `responses.TransportWebSocket` etc.) — drives
    /// the executor customization that the async wiring layer applies.
    pub transport: String,
}

impl CodexParams {
    /// Go outbound.go:62-65 — empty or legacy OpenAI v1 URL → [`CODEX_BASE_URL`].
    pub fn effective_base_url(&self) -> &str {
        if self.base_url.is_empty() || self.base_url == "https://api.openai.com/v1" {
            CODEX_BASE_URL
        } else {
            &self.base_url
        }
    }
}

/// Resolve the Codex Responses endpoint URL for a channel-configured base URL.
///
/// Composes [`CodexParams::effective_base_url`] with
/// [`resolve_outbound_url_for_endpoint`], mirroring Go's
/// `responses.buildFullRequestURL` invoked through the wrapped Responses
/// outbound (Go outbound.go:179 + responses/outbound.go:407-417). The
/// trailing `#` on [`CODEX_BASE_URL`] causes [`normalize_openai_base_url`]
/// to skip the `/v1` version segment, yielding
/// `https://chatgpt.com/backend-api/codex/responses` — exactly Go's
/// `codexAPIURL` (outbound.go:27).
pub fn resolve_codex_responses_url(params: &CodexParams) -> TransformerResult<String> {
    resolve_outbound_url_for_endpoint(
        params.effective_base_url().to_string(),
        None,
        false,
        DEFAULT_RESPONSES_PATH,
    )
}

// ---------------------------------------------------------------------------
// Outbound — Go `codex/outbound.go:30-233`.
//
// The Go `OutboundTransformer.TransformRequest` runs in two phases:
//   1. **Structured phase** (`outbound.go:106-187`): clone and mutate the
//      unified `*llm.Request` (stream / store / parallel_tool_calls /
//      transformer_metadata / image-model rewrite / strip token limits /
//      array_inputs), then delegate body construction to the wrapped
//      Responses outbound.
//   2. **HTTP decoration phase** (`outbound.go:189-232`): rewrite auth,
//      Accept, User-Agent, Originator, passthrough headers, Session_id
//      precedence, and the ChatGPT account-id header on the produced
//      `*httpclient.Request`.
//
// Phase 1 in Rust is captured by [`prepare_codex_request`] (operating on
// the fields Rust's `LlmRequest` currently models — the store / parallel
// tool_calls / max_tokens / reasoning_summary / transformer_metadata /
// transform_options mutations are pending the typed-port of those top-level
// fields, see *GAPS* notes inline). Phase 2 is captured by
// [`decorate_codex_http_request`] and is fully ported (every header
// mutation the Go tests assert on lives here).
//
// Not ported (async/executor wiring, mirrors the claudecode precedent):
//   * `codexExecutor` / `CustomizeExecutor` / `Stop` (outbound.go:265-374).
//   * `TransformResponse` / `TransformStream` / `AggregateStreamChunks`
//     (outbound.go:235-263) — pending the Responses response/stream module.
// ---------------------------------------------------------------------------

/// Facts the HTTP decoration phase needs from the structured phase, mirroring
/// the values Go's `TransformRequest` reads out of `llmReq.RawRequest.Headers`
/// (outbound.go:113-119) plus the JWT-extracted account id (outbound.go:127).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCodex {
    /// Trimmed inbound `Session_id` header (Go outbound.go:115). Used by the
    /// Session_id precedence waterfall (outbound.go:216-226).
    pub raw_session_id: String,
    /// Inbound `Originator` header (Go outbound.go:116). Empty → Conduit API
    /// originator is set instead (outbound.go:200-204).
    pub raw_originator: String,
    /// Inbound `User-Agent` header (Go outbound.go:117). Empty → UA is dropped
    /// and the HTTP client default (`conduit/1.0`) is observed on the wire.
    pub raw_user_agent: String,
    /// Inbound `X-Codex-Turn-Metadata` header (Go outbound.go:118). Used as
    /// the Session_id fallback (outbound.go:218).
    pub raw_turn_metadata: String,
    /// ChatGPT account id parsed from the access-token JWT (Go outbound.go:127,
    /// `ExtractChatGPTAccountIDFromJWT`). Empty → `Chatgpt-Account-Id` header
    /// is omitted (outbound.go:228-230).
    pub account_id: String,
    /// `true` when `llm_req.request_type == Image` (Go outbound.go:133). The
    /// decoration phase restores the original request_type / api_format on the
    /// HTTP request afterwards (Go outbound.go:184-187).
    pub is_image_request: bool,
    /// Snapshot of the inbound request_type — restored on the HTTP request
    /// for image payloads (Go outbound.go:184-185).
    pub original_request_type: RequestType,
    /// Snapshot of the inbound api_format — restored on the HTTP request for
    /// image payloads (Go outbound.go:186-187).
    pub original_api_format: ApiFormat,
}

/// Structured phase of Go `OutboundTransformer.TransformRequest`
/// (outbound.go:106-187). Mirrors the inbound-header sniff (outbound.go:113-
/// 119), the account-id extraction (outbound.go:127), the stream/store
/// mutation (outbound.go:138-143), and the image-model rewrite
/// (outbound.go:154-157).
///
/// *GAPS* (pending the typed-port of these `LlmRequest` fields; the Go
/// behavior is documented for the eventual wiring):
///   * `reqCopy.Store = false` (outbound.go:145) — Rust `LlmRequest` has no
///     `store` slot. Once added, zero it here.
///   * `reqCopy.ParallelToolCalls = true` (outbound.go:148) — same.
///   * `reqCopy.TransformerMetadata["include"] = ["reasoning.encrypted_content"]`
///     (outbound.go:160-163) and `reqCopy.ReasoningSummary = "auto"` default
///     (outbound.go:164-169) — pending the reasoning-summary field port.
///   * `reqCopy.MaxCompletionTokens = nil` / `MaxTokens = nil`
///     (outbound.go:172-173) and `reqCopy.Metadata = nil` (outbound.go:175) —
///     same.
///   * `reqCopy.TransformOptions.ArrayInputs = true` (outbound.go:177) —
///     pending the transform-options port.
pub fn prepare_codex_request(
    llm_req: &mut LlmRequest,
    inbound_headers: Option<&HeaderMap>,
    access_token: &str,
) -> PreparedCodex {
    // Go outbound.go:113-119 — sniff the four identity-bearing inbound headers.
    let (raw_session_id, raw_originator, raw_user_agent, raw_turn_metadata) = match inbound_headers
    {
        Some(headers) => (
            get_header(headers, SESSION_HEADER)
                .unwrap_or("")
                .to_string(),
            get_header(headers, "Originator").unwrap_or("").to_string(),
            get_header(headers, "User-Agent").unwrap_or("").to_string(),
            get_header(headers, TURN_METADATA_HEADER)
                .unwrap_or("")
                .to_string(),
        ),
        None => (String::new(), String::new(), String::new(), String::new()),
    };

    // Go outbound.go:127 — parse the account id without JWT signature
    // validation. Empty result → the header is omitted in decoration.
    let account_id = extract_chatgpt_account_id_from_jwt(access_token);

    let original_request_type = llm_req.request_type;
    let original_api_format = llm_req.api_format;
    let is_image_request = original_request_type == RequestType::Image;

    // Go outbound.go:138-143 — compact payloads stream=false, everything
    // else streams (Codex backend is SSE-only for non-compact requests).
    llm_req.stream = !matches!(original_request_type, RequestType::Compact);

    // Go outbound.go:154-157 — image requests are rewritten to ride the
    // Responses image-generation tool; the original model is attached to the
    // tool payload by the wrapped Responses outbound.
    if is_image_request {
        llm_req.model = Some(DEFAULT_IMAGE_MAIN_MODEL.to_string());
    }

    PreparedCodex {
        raw_session_id,
        raw_originator,
        raw_user_agent,
        raw_turn_metadata,
        account_id,
        is_image_request,
        original_request_type,
        original_api_format,
    }
}

/// HTTP decoration phase of Go `OutboundTransformer.TransformRequest`
/// (outbound.go:189-232). Operates on the [`HttpRequest`] produced by the
/// Responses body builder ([`build_openai_outbound_body`] + URL resolution).
///
/// * `http_req`      — the request to mutate.
/// * `access_token`  — the OAuth access token (Go `creds.AccessToken`).
/// * `is_compact`    — `true` when the inbound request_type was `Compact`
///   (drives the Accept header, Go outbound.go:192-196).
/// * `inbound_headers` — Go `llmReq.RawRequest.Headers` (raw passthrough
///   source). `None` mirrors the Go `rawHeaders == nil` branch.
/// * `prepared` — output of [`prepare_codex_request`].
/// * `context_session_id` — Go `shared.GetSessionID(ctx)` (the third-level
///   Session_id fallback, outbound.go:221). `None` is the
///   `context.Background()` case.
/// * `generated_session_id` — Go `uuid.NewString()` (outbound.go:224), the
///   final fallback when no other source supplies a session id. Passing it
///   in keeps this function pure; the caller generates it (or seeds a
///   deterministic value for tests).
pub fn decorate_codex_http_request(
    http_req: &mut HttpRequest,
    access_token: &str,
    is_compact: bool,
    inbound_headers: Option<&HeaderMap>,
    prepared: &PreparedCodex,
    context_session_id: Option<&str>,
    generated_session_id: &str,
) {
    // Go outbound.go:190 — overwrite auth with the OAuth bearer token.
    http_req.auth = Some(conduit_llm::model::HttpAuth {
        scheme: "bearer".to_string(),
        token: Some(access_token.to_string()),
        ..conduit_llm::model::HttpAuth::default()
    });

    // Go outbound.go:192-196 — compact requests expect JSON, everything else
    // expects SSE.
    let accept = if is_compact {
        "application/json"
    } else {
        "text/event-stream"
    };
    set_header(&mut http_req.headers, "Accept", accept.to_string());

    // Go outbound.go:198 — drop the inbound User-Agent unconditionally; if a
    // passthrough UA exists it is re-added below (outbound.go:206-208).
    del_header(&mut http_req.headers, "User-Agent");

    // Go outbound.go:200-204 — originator: inbound wins, else Conduit API default.
    let originator = if !prepared.raw_originator.is_empty() {
        prepared.raw_originator.as_str()
    } else {
        CONDUIT_ORIGINATOR
    };
    set_header(&mut http_req.headers, "Originator", originator.to_string());

    // Go outbound.go:206-208 — User-Agent passthrough (only when the inbound
    // request supplied one).
    if !prepared.raw_user_agent.is_empty() {
        set_header(
            &mut http_req.headers,
            "User-Agent",
            prepared.raw_user_agent.clone(),
        );
    }

    // Go outbound.go:210-214 — forward the Codex passthrough headers verbatim
    // (order preserved by Go's slice iteration; BTreeMap iteration here is
    // alphabetical which is fine — header order is not semantically meaningful
    // for HTTP/1.1 / HTTP/2).
    if let Some(raw_headers) = inbound_headers {
        for header_name in PASSTHROUGH_HEADERS {
            if let Some(value) =
                get_header(raw_headers, header_name).filter(|value| !value.is_empty())
            {
                set_header(&mut http_req.headers, header_name, value.to_string());
            }
        }
    }

    // Go outbound.go:216-226 — Session_id precedence waterfall:
    //   1. inbound `Session_id` header,
    //   2. session_id parsed from `X-Codex-Turn-Metadata`,
    //   3. existing `Session_id` on the outbound request (set by upstream
    //      middleware / base Responses transformer),
    //   4. `shared.GetSessionID(ctx)` — Rust: `context_session_id`,
    //   5. `uuid.NewString()` — Rust: `generated_session_id`.
    let resolved_session_id = if !prepared.raw_session_id.trim().is_empty() {
        prepared.raw_session_id.trim().to_string()
    } else {
        let from_turn = extract_session_id_from_turn_metadata(prepared.raw_turn_metadata.trim());
        if !from_turn.is_empty() {
            from_turn
        } else if let Some(existing) = get_header(&http_req.headers, SESSION_HEADER)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        {
            existing
        } else if let Some(ctx_id) = context_session_id.filter(|value| !value.is_empty()) {
            ctx_id.to_string()
        } else {
            generated_session_id.to_string()
        }
    };
    set_header(&mut http_req.headers, SESSION_HEADER, resolved_session_id);

    // Go outbound.go:228-230 — attach the ChatGPT account id when the JWT
    // carried one.
    if !prepared.account_id.is_empty() {
        set_header(
            &mut http_req.headers,
            CHATGPT_ACCOUNT_ID_HEADER,
            prepared.account_id.clone(),
        );
    }

    // Go outbound.go:184-187 — image requests ride the Responses image tool
    // but the downstream HTTP request keeps the original request_type /
    // api_format so the response path picks the image branch.
    if prepared.is_image_request {
        http_req.request_type = Some(prepared.original_request_type);
        http_req.api_format = Some(prepared.original_api_format);
    }
}

/// Full request build mirroring Go `OutboundTransformer.TransformRequest`
/// (outbound.go:101-232): resolve the OAuth token, run the structured phase,
/// serialize via the base OpenAI Responses outbound ([`build_openai_outbound_body`]
/// + [`resolve_codex_responses_url`]), then apply the Codex decorations.
///
/// The inbound `LlmRequest` is mutated in place (matching Go's shallow-clone
/// behavior — `reqCopy := *llmReq`).
pub fn build_codex_http_request(
    llm_req: &mut LlmRequest,
    inbound_headers: Option<&HeaderMap>,
    params: &CodexParams,
    tokens: &dyn TokenGetter,
    context_session_id: Option<&str>,
    generated_session_id: &str,
) -> TransformerResult<HttpRequest> {
    // Go outbound.go:121-123 — token first (before any mutation).
    let creds = tokens.get()?;
    let access_token = creds.access_token.clone();

    let original_request_type = llm_req.request_type;
    let is_compact = original_request_type == RequestType::Compact;

    // Go outbound.go:106-187 — structured phase.
    let prepared = prepare_codex_request(llm_req, inbound_headers, &access_token);

    // Go outbound.go:179 — base Responses transformer body. The Rust
    // `build_openai_outbound_body` already handles Responses payloads
    // (including the compact branch) and injects the top-level `model` /
    // `stream` / `instructions` / `tools` / `reasoning` / `include` fields
    // present on `LlmRequest` + its payload.
    let body = build_openai_outbound_body(llm_req)?;

    // Go outbound.go:179 + responses/outbound.go:407-417 — URL resolution.
    let url = resolve_codex_responses_url(params)?;

    let mut http_req = HttpRequest {
        method: "POST".to_string(),
        url: Some(url),
        path: DEFAULT_RESPONSES_PATH.to_string(),
        headers: HeaderMap::new(),
        json_body: Some(body),
        ..HttpRequest::default()
    };

    // Go outbound.go:189-232 — HTTP decoration phase.
    decorate_codex_http_request(
        &mut http_req,
        &access_token,
        is_compact,
        inbound_headers,
        &prepared,
        context_session_id,
        generated_session_id,
    );

    Ok(http_req)
}

/// Go `SessionHeader` (headers.go:10). Go's `http.Header` canonicalization
/// leaves `Session_id` unchanged (`_` is a plain token char).
pub const SESSION_HEADER: &str = "Session_id";
/// Go `TurnMetadataHeader` (headers.go:11).
pub const TURN_METADATA_HEADER: &str = "X-Codex-Turn-Metadata";
/// Go `WindowIDHeader` (headers.go:12).
pub const WINDOW_ID_HEADER: &str = "X-Codex-Window-Id";
/// Go `ClientRequestIDHeader` (headers.go:13).
pub const CLIENT_REQUEST_ID_HEADER: &str = "X-Client-Request-Id";
/// Go `BetaFeaturesHeader` (headers.go:14).
pub const BETA_FEATURES_HEADER: &str = "X-Codex-Beta-Features";

/// Mirrors Go `codex.TurnMetadata` (headers.go:17-19). Go tag verbatim:
/// `session_id` (no omitempty). Unknown extra fields (e.g. `turn_id`) are
/// ignored on decode, matching Go `json.Unmarshal` into the partial struct.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMetadata {
    /// Go `SessionID string \`json:"session_id"\``.
    #[serde(default)]
    pub session_id: String,
}

/// Go `PassthroughHeaders` (headers.go:21-26) — inbound Codex headers
/// forwarded verbatim to the ChatGPT backend (order preserved).
pub const PASSTHROUGH_HEADERS: [&str; 4] = [
    TURN_METADATA_HEADER,
    WINDOW_ID_HEADER,
    CLIENT_REQUEST_ID_HEADER,
    BETA_FEATURES_HEADER,
];

/// Case-insensitive header get, mirroring Go `http.Header.Get` over the
/// crate's plain `BTreeMap<String, String>` header map.
fn get_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Case-insensitive header set, mirroring Go `http.Header.Set` (replaces any
/// case-variant of the key, stores the canonical name passed in).
fn set_header(headers: &mut HeaderMap, name: &str, value: String) {
    headers.retain(|key, _| !key.eq_ignore_ascii_case(name));
    headers.insert(name.to_string(), value);
}

/// Case-insensitive header delete, mirroring Go `http.Header.Del`.
fn del_header(headers: &mut HeaderMap, name: &str) {
    headers.retain(|key, _| !key.eq_ignore_ascii_case(name));
}

/// Mirrors Go `codex.ExtractSessionIDFromTurnMetadata` (headers.go:28-39):
/// empty input or JSON decode failure → `""`; otherwise the trimmed
/// `session_id` field of the turn-metadata payload.
pub fn extract_session_id_from_turn_metadata(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }

    match serde_json::from_str::<TurnMetadata>(raw) {
        Ok(payload) => payload.session_id.trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Mirrors Go `codex.GetSessionIDFromHeaders` (headers.go:41-52): a trimmed
/// non-empty `Session_id` header wins; otherwise fall back to the session id
/// embedded in the (trimmed) `X-Codex-Turn-Metadata` header. `None` headers
/// mirror the Go `headers == nil` guard.
pub fn get_session_id_from_headers(headers: Option<&HeaderMap>) -> String {
    let Some(headers) = headers else {
        return String::new();
    };

    let session_id = get_header(headers, SESSION_HEADER).unwrap_or("").trim();
    if !session_id.is_empty() {
        return session_id.to_string();
    }

    extract_session_id_from_turn_metadata(
        get_header(headers, TURN_METADATA_HEADER)
            .unwrap_or("")
            .trim(),
    )
}

// ---------------------------------------------------------------------------
// Utils — Go `codex/utils.go:1-70`.
// ---------------------------------------------------------------------------

/// Decode one base64url char to its 6-bit value, or `None` if not in the
/// url alphabet. Padding (`=`) is rejected — JWT payloads use RawURLEncoding.
fn b64url_decode_char(c: u8) -> Option<u8> {
    if c.is_ascii_uppercase() {
        Some(c - b'A')
    } else if c.is_ascii_lowercase() {
        Some(c - b'a' + 26)
    } else if c.is_ascii_digit() {
        Some(c - b'0' + 52)
    } else if c == b'-' {
        Some(62)
    } else if c == b'_' {
        Some(63)
    } else {
        None
    }
}

/// Strict base64url (no padding) decode. Mirrors Go's
/// `base64.RawURLEncoding.DecodeString`:
///   * only `[A-Za-z0-9-_]`, no `=` padding;
///   * length modulo 4 determines how many bytes the final block yields
///     (1 → invalid, 2 → 1 byte, 3 → 2 bytes, 0 → no trailing block).
fn b64url_decode(input: &str) -> Option<Vec<u8>> {
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    for &b in bytes {
        if b64url_decode_char(b).is_none() {
            return None;
        }
    }
    let full_blocks = bytes.len() / 4;
    let rem = bytes.len() % 4;
    let mut out = Vec::with_capacity(full_blocks * 3 + rem.saturating_sub(1) / 2 + 1);
    for block in 0..full_blocks {
        let off = block * 4;
        let v0 = b64url_decode_char(bytes[off])?;
        let v1 = b64url_decode_char(bytes[off + 1])?;
        let v2 = b64url_decode_char(bytes[off + 2])?;
        let v3 = b64url_decode_char(bytes[off + 3])?;
        let triple = ((v0 as u32) << 18) | ((v1 as u32) << 12) | ((v2 as u32) << 6) | (v3 as u32);
        out.push(((triple >> 16) & 0xFF) as u8);
        out.push(((triple >> 8) & 0xFF) as u8);
        out.push((triple & 0xFF) as u8);
    }
    if rem == 2 {
        let v0 = b64url_decode_char(bytes[full_blocks * 4])?;
        let v1 = b64url_decode_char(bytes[full_blocks * 4 + 1])?;
        let pair = ((v0 as u32) << 6) | (v1 as u32);
        out.push(((pair >> 4) & 0xFF) as u8);
    } else if rem == 3 {
        let v0 = b64url_decode_char(bytes[full_blocks * 4])?;
        let v1 = b64url_decode_char(bytes[full_blocks * 4 + 1])?;
        let v2 = b64url_decode_char(bytes[full_blocks * 4 + 2])?;
        let triple = ((v0 as u32) << 12) | ((v1 as u32) << 6) | (v2 as u32);
        out.push(((triple >> 10) & 0xFF) as u8);
        out.push(((triple >> 2) & 0xFF) as u8);
    } else if rem == 1 {
        return None;
    }
    Some(out)
}

/// Mirrors Go `codex.ExtractChatGPTAccountIDFromJWT` (utils.go:9-33): parse
/// the JWT **without signature validation** (Go uses
/// `jwt.ParseUnverified`), then dig out
/// `claims["https://api.openai.com/auth"]["chatgpt_account_id"]` as a string.
/// Any decode failure or missing key returns `""`.
pub fn extract_chatgpt_account_id_from_jwt(token_str: &str) -> String {
    let parts: Vec<&str> = token_str.split('.').collect();
    if parts.len() < 2 {
        return String::new();
    }
    let Some(decoded) = b64url_decode(parts[1]) else {
        return String::new();
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&decoded) else {
        return String::new();
    };
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Mirrors Go `codex.isCodexCLIVersion` (utils.go:35-70): non-empty (after
/// trim), contains at least one `.`, every char is in `[0-9a-zA-Z.\-+_ ]`.
pub fn is_codex_cli_version(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    let mut dot = false;
    for &c in v.as_bytes() {
        match c {
            b'.' => dot = true,
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'+' | b'_' => {}
            _ => return false,
        }
    }
    dot
}

// ---------------------------------------------------------------------------
// Token — Go `codex/token.go:1-80` + the form-encoded strategy it defaults to
// (Go `oauth.FormEncodedStrategy`, `llm/oauth/exchange_strategy.go:23-87`).
// The shared credential/refresh decision pieces (`OAuthCredentials`,
// `parse_token_response`, `parse_refresh_response`, `is_expired`,
// `should_refresh`, `TokenGetter`) are reused from `crate::claudecode`.
// ---------------------------------------------------------------------------

/// Mirrors Go `codex.DefaultTokenURLs` (token.go:14-18) — the production
/// OpenAI OAuth endpoints.
pub fn default_token_urls() -> OAuthUrls {
    OAuthUrls {
        authorize_url: AUTHORIZE_URL.to_string(),
        token_url: TOKEN_URL.to_string(),
    }
}

/// Mirrors the nested `Tokens` struct of Go `codex.AuthJSON` (token.go:28-32).
/// Go tags verbatim: `access_token` (no omitempty), `refresh_token,omitempty`,
/// `id_token,omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthJsonTokens {
    #[serde(default)]
    pub access_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id_token: String,
}

/// Mirrors Go `codex.AuthJSON` (token.go:26-33) — the Codex CLI `auth.json`
/// shape. Go tags verbatim: `last_refresh,omitempty`, `tokens` (no omitempty).
/// Unknown fields (e.g. `auth_mode`) are ignored on decode, matching Go.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthJson {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_refresh: String,
    #[serde(default)]
    pub tokens: AuthJsonTokens,
}

/// Mirrors Go `codex.DecodeAuthJSON` (token.go:35-71):
/// * trims, rejects empty input (`"empty auth json"`),
/// * JSON decode failure propagates,
/// * rejects a missing/whitespace `access_token` (`"access_token is empty"`),
/// * credentials get the Codex `ClientID`, `token_type = "bearer"`, and the
///   whitespace-split `Scopes` list,
/// * a parseable RFC3339(Nano) `last_refresh` sets `expires_at =
///   last_refresh + 1h` (parse errors ignored, Go token.go:59-64),
/// * a refresh token with a still-zero expiry assumes `now + 1h` (Go
///   token.go:66-68 uses `time.Now()`; passed explicitly to stay pure).
pub fn decode_auth_json(
    raw: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> TransformerResult<OAuthCredentials> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConduitError::invalid_request("empty auth json"));
    }

    let auth_json: AuthJson = serde_json::from_str(trimmed)
        .map_err(|err| ConduitError::invalid_request(err.to_string()))?;

    if auth_json.tokens.access_token.trim().is_empty() {
        return Err(ConduitError::invalid_request("access_token is empty"));
    }

    let mut creds = OAuthCredentials {
        client_id: CLIENT_ID.to_string(),
        access_token: auth_json.tokens.access_token,
        refresh_token: auth_json.tokens.refresh_token,
        id_token: auth_json.tokens.id_token,
        token_type: "bearer".to_string(),
        scopes: SCOPES.split_whitespace().map(str::to_string).collect(),
        ..OAuthCredentials::default()
    };

    // Go token.go:59-64 — RFC3339Nano parse; errors are silently ignored.
    if !auth_json.last_refresh.is_empty()
        && let Ok(last_refresh) = chrono::DateTime::parse_from_rfc3339(&auth_json.last_refresh)
    {
        creds.expires_at = last_refresh.with_timezone(&chrono::Utc) + chrono::Duration::hours(1);
    }

    // Go token.go:66-68 — refreshable but no expiry → assume 1 hour.
    if !creds.refresh_token.is_empty() && creds.expires_at_is_zero() {
        creds.expires_at = now + chrono::Duration::hours(1);
    }

    Ok(creds)
}

/// Go `url.QueryEscape` — unreserved chars (`A-Za-z0-9`, `-`, `_`, `.`, `~`)
/// pass through, space becomes `+`, everything else `%XX` (uppercase hex).
fn query_escape(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    out
}

/// Go `url.Values.Encode()` — `key=value` pairs joined by `&`, keys sorted
/// alphabetically, keys and values query-escaped.
fn form_encode(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, &str)> = pairs.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    sorted
        .iter()
        .map(|(key, value)| format!("{}={}", query_escape(key), query_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

/// Shared headers of the form-encoded token requests
/// (exchange_strategy.go:42-48 / 73-79). `codex.NewTokenProvider` (token.go:
/// 73-80) passes no `UserAgent`, so no User-Agent header is set here — the Go
/// HTTP client injects its `conduit/1.0` default at execution time
/// (`llm/httpclient/client.go:374`; Rust: `conduit_llm::http::client`).
fn form_token_request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers
}

/// Build the authorization-code exchange request. Combines Go
/// `TokenProvider.Exchange`'s parameter validation (token_provider.go:105-119,
/// Go error strings verbatim) with `FormEncodedStrategy.BuildExchangeRequest`
/// (exchange_strategy.go:34-56). Unlike the Claude Code JSON strategy, the
/// form strategy **ignores `params.state`** (Go sets only the five standard
/// fields). `token_url` mirrors the Go strategy signature; production flows
/// pass [`default_token_urls`]`().token_url`.
pub fn build_exchange_request(
    params: &ExchangeParams,
    token_url: &str,
) -> TransformerResult<HttpRequest> {
    if params.code.is_empty() {
        return Err(ConduitError::invalid_request("code is empty"));
    }
    if params.code_verifier.is_empty() {
        return Err(ConduitError::invalid_request("code_verifier is empty"));
    }
    if params.client_id.is_empty() {
        return Err(ConduitError::invalid_request("client_id is empty"));
    }
    if params.redirect_uri.is_empty() {
        return Err(ConduitError::invalid_request("redirect_uri is empty"));
    }

    let body = form_encode(&[
        ("grant_type", "authorization_code"),
        ("client_id", &params.client_id),
        ("code", &params.code),
        ("redirect_uri", &params.redirect_uri),
        ("code_verifier", &params.code_verifier),
    ]);

    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(token_url.to_string()),
        headers: form_token_request_headers(),
        body: Some(body),
        ..HttpRequest::default()
    })
}

/// Build the refresh-token request. Mirrors
/// `FormEncodedStrategy.BuildRefreshRequest` (exchange_strategy.go:59-87) —
/// the `refresh_token is empty` guard is the Go one; `client_id` is always
/// included, even when empty. Response parsing reuses
/// [`crate::claudecode::parse_refresh_response`] (Go `TokenProvider.refresh`
/// preserves the old refresh token when the response omits one).
pub fn build_refresh_request(
    creds: &OAuthCredentials,
    token_url: &str,
) -> TransformerResult<HttpRequest> {
    if creds.refresh_token.is_empty() {
        return Err(ConduitError::invalid_request("refresh_token is empty"));
    }

    let body = form_encode(&[
        ("grant_type", "refresh_token"),
        ("client_id", &creds.client_id),
        ("refresh_token", &creds.refresh_token),
    ]);

    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(token_url.to_string()),
        headers: form_token_request_headers(),
        body: Some(body),
        ..HttpRequest::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // Constants pins — Go constants.go golden values.
    #[test]
    fn constants_match_go_constants_go() {
        assert_eq!(default_models().len(), 14);
        assert_eq!(DEFAULT_MODELS[0], "gpt-5");
        assert_eq!(DEFAULT_MODELS[1], "gpt-5-codex");
        assert_eq!(DEFAULT_MODELS[13], "gpt-5.5");
        assert_eq!(DEFAULT_IMAGE_MAIN_MODEL, "gpt-5.4-mini");
        assert_eq!(CONDUIT_ORIGINATOR, "conduit");
        assert_eq!(AUTHORIZE_URL, "https://auth.openai.com/oauth/authorize");
        assert_eq!(TOKEN_URL, "https://auth.openai.com/oauth/token");
        assert_eq!(CLIENT_ID, "app_EMoamEEZ73f0CkXaXp7hrann");
        assert_eq!(REDIRECT_URI, "http://localhost:1455/auth/callback");
        assert_eq!(SCOPES, "openid profile email offline_access");
        assert_eq!(CODEX_API_FORMAT, ApiFormat::OpenAiResponses);
    }

    // ----- extract_session_id_from_turn_metadata -------------------------------
    // Mirrors Go codex_simulator_test.go SessionIDPrecedence subtests
    // (lines 120-153) — the turn-metadata fallback branch.

    #[test]
    fn extract_session_id_empty_raw_returns_empty() -> TestResult {
        assert_eq!(extract_session_id_from_turn_metadata(""), "");
        Ok(())
    }

    #[test]
    fn extract_session_id_invalid_json_returns_empty() -> TestResult {
        assert_eq!(
            extract_session_id_from_turn_metadata(r#"{"session_id":"#),
            ""
        );
        Ok(())
    }

    #[test]
    fn extract_session_id_returns_trimmed_session_id() -> TestResult {
        assert_eq!(
            extract_session_id_from_turn_metadata(
                r#"{"session_id":"  turn-session  ","turn_id":"turn-123"}"#
            ),
            "turn-session"
        );
        Ok(())
    }

    // ----- get_session_id_from_headers -----------------------------------------
    // Mirrors Go codex/headers.go:41-52 precedence: Session_id header wins,
    // otherwise the turn-metadata session_id is the fallback.

    #[test]
    fn get_session_id_from_nil_headers_returns_empty() -> TestResult {
        assert_eq!(get_session_id_from_headers(None), "");
        Ok(())
    }

    #[test]
    fn get_session_id_prefers_session_header_over_turn_metadata() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER.to_string(), "header-session".to_string());
        headers.insert(
            TURN_METADATA_HEADER.to_string(),
            r#"{"session_id":"turn-session"}"#.to_string(),
        );
        assert_eq!(
            get_session_id_from_headers(Some(&headers)),
            "header-session"
        );
        Ok(())
    }

    #[test]
    fn get_session_id_falls_back_to_turn_metadata() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert(
            TURN_METADATA_HEADER.to_string(),
            r#"{"session_id":"turn-session","turn_id":"turn-123"}"#.to_string(),
        );
        assert_eq!(get_session_id_from_headers(Some(&headers)), "turn-session");
        Ok(())
    }

    #[test]
    fn get_session_id_returns_empty_when_neither_source_has_it() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert(
            TURN_METADATA_HEADER.to_string(),
            r#"{"session_id":"  "}"#.to_string(),
        );
        assert_eq!(get_session_id_from_headers(Some(&headers)), "");
        Ok(())
    }

    // ----- is_codex_cli_version ------------------------------------------------
    // Mirrors Go codex/utils.go:35-70 character-class + dot requirement.

    #[test]
    fn is_codex_cli_version_rejects_empty_and_trimmed_empty() -> TestResult {
        assert!(!is_codex_cli_version(""));
        assert!(!is_codex_cli_version("   "));
        Ok(())
    }

    #[test]
    fn is_codex_cli_version_requires_at_least_one_dot() -> TestResult {
        assert!(!is_codex_cli_version("codex"));
        assert!(is_codex_cli_version("0.50.0"));
        Ok(())
    }

    #[test]
    fn is_codex_cli_version_accepts_letters_digits_dash_plus_underscore() -> TestResult {
        // Per Go utils.go:42-67 the allowed set is [0-9a-zA-Z.-+_]. Space is
        // *not* in the set; the inbound string is only trimmed at the ends.
        assert!(is_codex_cli_version("codex_cli_rs-0.50.0"));
        assert!(is_codex_cli_version("1.2.3-alpha+build"));
        assert!(is_codex_cli_version("  9.9.9  "));
        Ok(())
    }

    #[test]
    fn is_codex_cli_version_rejects_other_punctuation() -> TestResult {
        assert!(!is_codex_cli_version("0.50.0!"));
        assert!(!is_codex_cli_version("a.b/c"));
        Ok(())
    }

    // ----- extract_chatgpt_account_id_from_jwt ---------------------------------
    // Mirrors Go codex/utils.go:9-33. We craft a minimal unsigned JWT whose
    // payload carries the `https://api.openai.com/auth.chatgpt_account_id`
    // claim — exactly the shape the Go `testAccessTokenWithAccountID` helper
    // produces (codex_simulator_test.go:33-46) minus the signature.

    /// Build an unsigned JWT (header.payload.) with a JSON object payload.
    /// Mirrors the relevant slice of `testAccessTokenWithAccountID`.
    fn make_unsigned_jwt(payload: &Value) -> String {
        let header = serde_json::json!({"alg":"HS256","typ":"JWT"});
        let header_b64 = b64url_encode(serde_json::to_vec(&header).unwrap_or_default().as_slice());
        let payload_b64 = b64url_encode(serde_json::to_vec(payload).unwrap_or_default().as_slice());
        format!("{header_b64}.{payload_b64}.")
    }

    fn b64url_encode(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() >= 2 {
                out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            }
            if chunk.len() >= 3 {
                out.push(ALPHABET[(triple & 0x3F) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn extract_account_id_returns_empty_for_malformed_token() -> TestResult {
        assert_eq!(extract_chatgpt_account_id_from_jwt("not-a-jwt"), "");
        assert_eq!(extract_chatgpt_account_id_from_jwt("a.b"), "");
        assert_eq!(extract_chatgpt_account_id_from_jwt(""), "");
        Ok(())
    }

    #[test]
    fn extract_account_id_returns_empty_when_claim_missing() -> TestResult {
        let payload = serde_json::json!({"sub":"user-123"});
        let token = make_unsigned_jwt(&payload);
        assert_eq!(extract_chatgpt_account_id_from_jwt(&token), "");
        Ok(())
    }

    #[test]
    fn extract_account_id_returns_chatgpt_account_id() -> TestResult {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_test"}
        });
        let token = make_unsigned_jwt(&payload);
        assert_eq!(extract_chatgpt_account_id_from_jwt(&token), "acct_test");
        Ok(())
    }

    // ----- decode_auth_json ----------------------------------------------------
    // Mirrors Go codex/token_test.go:10-28 `TestDecodeAuthJSON` verbatim — the
    // Codex CLI auth.json shape (last_refresh + tokens) plus the
    // `auth_mode` unknown field that Go ignores.

    #[test]
    fn decode_auth_json_empty_returns_error() -> TestResult {
        let now = chrono::Utc::now();
        match decode_auth_json("   ", now) {
            Ok(_) => panic!("expected error, got Ok"),
            Err(err) => assert!(err.to_string().contains("empty auth json")),
        }
        Ok(())
    }

    #[test]
    fn decode_auth_json_empty_access_token_returns_error() -> TestResult {
        let now = chrono::Utc::now();
        match decode_auth_json(r#"{"tokens":{"access_token":"  "}}"#, now) {
            Ok(_) => panic!("expected error, got Ok"),
            Err(err) => assert!(err.to_string().contains("access_token is empty")),
        }
        Ok(())
    }

    #[test]
    fn decode_auth_json_mirrors_go_token_test_go() -> TestResult {
        let now = chrono::Utc::now();
        let creds = decode_auth_json(
            r#"{
                "auth_mode":"chatgpt",
                "last_refresh":"2026-04-17T08:58:36.389Z",
                "tokens":{
                    "access_token":"access",
                    "refresh_token":"refresh",
                    "id_token":"id"
                }
            }"#,
            now,
        )?;

        assert_eq!(creds.client_id, CLIENT_ID);
        assert_eq!(creds.access_token, "access");
        assert_eq!(creds.refresh_token, "refresh");
        assert_eq!(creds.id_token, "id");
        assert_eq!(creds.token_type, "bearer");
        assert_eq!(
            creds.scopes,
            vec!["openid", "profile", "email", "offline_access"]
        );

        // Go token_test.go:27 — ExpiresAt = last_refresh + 1h.
        let expected = chrono::DateTime::parse_from_rfc3339("2026-04-17T08:58:36.389Z")?
            .with_timezone(&chrono::Utc)
            + chrono::Duration::hours(1);
        assert_eq!(creds.expires_at, expected);
        Ok(())
    }

    #[test]
    fn decode_auth_json_refresh_without_expiry_assumes_one_hour() -> TestResult {
        let now = chrono::Utc::now();
        let creds = decode_auth_json(
            r#"{"tokens":{"access_token":"a","refresh_token":"r"}}"#,
            now,
        )?;
        // Go token.go:66-68 — refreshable but no expiry → now + 1h.
        assert!(creds.expires_at >= now + chrono::Duration::minutes(59));
        assert!(creds.expires_at <= now + chrono::Duration::minutes(61));
        Ok(())
    }

    // ----- build_exchange_request / build_refresh_request ----------------------
    // Mirrors the form-encoded strategy's wire shape that the Codex token
    // provider defaults to (Go exchange_strategy.go:34-87).

    #[test]
    fn build_exchange_request_validates_required_fields() -> TestResult {
        let params = ExchangeParams::default();
        match build_exchange_request(&params, TOKEN_URL) {
            Ok(_) => panic!("expected error, got Ok"),
            Err(err) => assert!(err.to_string().contains("code is empty")),
        }
        Ok(())
    }

    #[test]
    fn build_exchange_request_form_encodes_body() -> TestResult {
        let params = ExchangeParams {
            code: "the-code".to_string(),
            code_verifier: "verifier".to_string(),
            client_id: CLIENT_ID.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
            ..ExchangeParams::default()
        };
        let req = build_exchange_request(&params, TOKEN_URL)?;
        assert_eq!(req.method, "POST");
        assert_eq!(req.url.as_deref(), Some(TOKEN_URL));
        assert_eq!(
            req.headers.get("Content-Type").map(String::as_str),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            req.headers.get("Accept").map(String::as_str),
            Some("application/json")
        );
        let body = String::from_utf8(req.body.unwrap_or_default())?;
        // Form-encoded strategy sorts keys alphabetically (Go url.Values.Encode).
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=the-code"));
        assert!(body.contains("code_verifier=verifier"));
        assert!(body.contains(&format!("client_id={}", CLIENT_ID)));
        // REDIRECT_URI carries `://` and `/` which query-escape percent-encodes.
        assert!(body.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        Ok(())
    }

    #[test]
    fn build_refresh_request_requires_refresh_token() -> TestResult {
        let creds = OAuthCredentials::default();
        match build_refresh_request(&creds, TOKEN_URL) {
            Ok(_) => panic!("expected error, got Ok"),
            Err(err) => assert!(err.to_string().contains("refresh_token is empty")),
        }
        Ok(())
    }

    #[test]
    fn build_refresh_request_form_encodes_body() -> TestResult {
        let creds = OAuthCredentials {
            client_id: CLIENT_ID.to_string(),
            refresh_token: "rt-value".to_string(),
            ..OAuthCredentials::default()
        };
        let req = build_refresh_request(&creds, TOKEN_URL)?;
        let body = String::from_utf8(req.body.unwrap_or_default())?;
        assert!(body.contains("grant_type=refresh_token"));
        assert!(body.contains("refresh_token=rt-value"));
        assert!(body.contains(&format!("client_id={}", CLIENT_ID)));
        Ok(())
    }

    // ----- resolve_codex_responses_url -----------------------------------------
    // Mirrors the URL Go codex/outbound.go produces when wrapping the Responses
    // transformer: codexBaseURL (trailing `#` → no /v1) + /responses path
    // == codexAPIURL.

    #[test]
    fn resolve_codex_responses_url_default_base() -> TestResult {
        let url = resolve_codex_responses_url(&CodexParams::default())?;
        assert_eq!(url, CODEX_API_URL);
        Ok(())
    }

    #[test]
    fn resolve_codex_responses_url_legacy_openai_v1_falls_back() -> TestResult {
        let params = CodexParams {
            base_url: "https://api.openai.com/v1".to_string(),
            ..CodexParams::default()
        };
        let url = resolve_codex_responses_url(&params)?;
        assert_eq!(url, CODEX_API_URL);
        Ok(())
    }

    #[test]
    fn codex_params_effective_base_url_uses_override() -> TestResult {
        let params = CodexParams {
            base_url: "https://example.test/codex".to_string(),
            ..CodexParams::default()
        };
        assert_eq!(params.effective_base_url(), "https://example.test/codex");
        Ok(())
    }

    // ----- prepare_codex_request ----------------------------------------------
    // Mirrors Go outbound.go:106-187 structured-phase mutations observable
    // from Rust's LlmRequest: stream flag + image-model rewrite.

    fn make_chat_llm_request() -> LlmRequest {
        use conduit_llm::model::{ChatRequest, LlmRequestPayload};
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-5-codex".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    #[test]
    fn prepare_codex_request_forces_stream_for_chat() -> TestResult {
        let mut llm_req = make_chat_llm_request();
        let prepared = prepare_codex_request(&mut llm_req, None, "");
        assert!(llm_req.stream, "chat requests must force stream=true");
        assert!(!prepared.is_image_request);
        Ok(())
    }

    #[test]
    fn prepare_codex_request_forces_non_stream_for_compact() -> TestResult {
        let mut llm_req = make_chat_llm_request();
        llm_req.request_type = RequestType::Compact;
        let prepared = prepare_codex_request(&mut llm_req, None, "");
        assert!(!llm_req.stream, "compact requests must force stream=false");
        assert!(!prepared.is_image_request);
        Ok(())
    }

    #[test]
    fn prepare_codex_request_rewrites_image_model() -> TestResult {
        let mut llm_req = make_chat_llm_request();
        llm_req.request_type = RequestType::Image;
        llm_req.api_format = ApiFormat::OpenAiImageGeneration;
        llm_req.model = Some("gpt-image-2".to_string());
        let prepared = prepare_codex_request(&mut llm_req, None, "");
        assert!(prepared.is_image_request);
        assert_eq!(llm_req.model.as_deref(), Some(DEFAULT_IMAGE_MAIN_MODEL));
        assert_eq!(prepared.original_request_type, RequestType::Image);
        Ok(())
    }

    #[test]
    fn prepare_codex_request_reads_inbound_identity_headers() -> TestResult {
        let mut llm_req = make_chat_llm_request();
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER.to_string(), "inbound-session".to_string());
        headers.insert("Originator".to_string(), "inbound-originator".to_string());
        headers.insert("User-Agent".to_string(), "inbound-ua/1.0".to_string());
        headers.insert(
            TURN_METADATA_HEADER.to_string(),
            r#"{"session_id":"turn-session"}"#.to_string(),
        );
        let prepared = prepare_codex_request(&mut llm_req, Some(&headers), "");
        assert_eq!(prepared.raw_session_id, "inbound-session");
        assert_eq!(prepared.raw_originator, "inbound-originator");
        assert_eq!(prepared.raw_user_agent, "inbound-ua/1.0");
        assert_eq!(
            prepared.raw_turn_metadata,
            r#"{"session_id":"turn-session"}"#
        );
        Ok(())
    }

    // ----- decorate_codex_http_request ----------------------------------------
    // Mirrors Go codex_simulator_test.go::TestCodexOutbound_MinimalIdentityHeaders
    // (lines 48-72) and the SessionIDPrecedence family. Each test constructs a
    // minimal HttpRequest + PreparedCodex and asserts the post-decoration
    // header state.

    fn make_prepared(
        session_id: &str,
        originator: &str,
        user_agent: &str,
        turn_metadata: &str,
        account_id: &str,
    ) -> PreparedCodex {
        PreparedCodex {
            raw_session_id: session_id.to_string(),
            raw_originator: originator.to_string(),
            raw_user_agent: user_agent.to_string(),
            raw_turn_metadata: turn_metadata.to_string(),
            account_id: account_id.to_string(),
            is_image_request: false,
            original_request_type: RequestType::Chat,
            original_api_format: ApiFormat::OpenAiChatCompletions,
        }
    }

    fn make_bare_http_request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            url: Some(CODEX_API_URL.to_string()),
            path: "/responses".to_string(),
            headers: HeaderMap::new(),
            json_body: Some(serde_json::json!({})),
            ..HttpRequest::default()
        }
    }

    /// Mirrors Go `TestCodexOutbound_MinimalIdentityHeaders` — no inbound
    /// identity headers → Conduit API originator + conduit/1.0 UA (the HTTP client
    /// default; we verify the UA header is dropped, which is what lets the
    /// client default win on the wire).
    #[test]
    fn decorate_minimal_identity_headers_mirrors_go_test() -> TestResult {
        let mut http_req = make_bare_http_request();
        let prepared = make_prepared("", "", "", "", "acct_test");
        decorate_codex_http_request(
            &mut http_req,
            "access-token",
            false,
            None,
            &prepared,
            None,
            "generated-session",
        );

        assert_eq!(
            http_req.headers.get("Accept").map(String::as_str),
            Some("text/event-stream")
        );
        assert_eq!(
            http_req.headers.get("Originator").map(String::as_str),
            Some(CONDUIT_ORIGINATOR)
        );
        // No inbound UA → UA header is dropped (client default fills it).
        assert!(!http_req.headers.contains_key("User-Agent"));
        assert_eq!(
            http_req
                .headers
                .get(CHATGPT_ACCOUNT_ID_HEADER)
                .map(String::as_str),
            Some("acct_test")
        );
        // Bearer auth with the access token.
        let auth = http_req.auth.as_ref().ok_or("auth must be set")?;
        assert_eq!(auth.scheme, "bearer");
        assert_eq!(auth.token.as_deref(), Some("access-token"));
        Ok(())
    }

    /// Mirrors Go `TestCodexOutbound_AllowsInboundIdentityOverrides` — inbound
    /// Originator + User-Agent win over the Conduit API defaults.
    #[test]
    fn decorate_inbound_identity_overrides_mirrors_go_test() -> TestResult {
        let mut http_req = make_bare_http_request();
        let prepared = make_prepared(
            "",
            "codex_cli_rs",
            "codex_cli_rs/0.50.0 (macOS 14.0.0; arm64) Terminal",
            "",
            "acct_test",
        );
        decorate_codex_http_request(
            &mut http_req,
            "access-token",
            false,
            None,
            &prepared,
            None,
            "generated-session",
        );
        assert_eq!(
            http_req.headers.get("Originator").map(String::as_str),
            Some("codex_cli_rs")
        );
        assert_eq!(
            http_req.headers.get("User-Agent").map(String::as_str),
            Some("codex_cli_rs/0.50.0 (macOS 14.0.0; arm64) Terminal")
        );
        Ok(())
    }

    /// Mirrors Go `TestCodexOutbound_PassthroughModernCodexHeaders`.
    #[test]
    fn decorate_passthrough_modern_codex_headers_mirrors_go_test() -> TestResult {
        let mut http_req = make_bare_http_request();
        let prepared = make_prepared("", "", "", "", "");
        let mut inbound = HeaderMap::new();
        inbound.insert(
            TURN_METADATA_HEADER.to_string(),
            r#"{"session_id":"turn-session","turn_id":"turn-123"}"#.to_string(),
        );
        inbound.insert(WINDOW_ID_HEADER.to_string(), "window-123".to_string());
        inbound.insert(
            CLIENT_REQUEST_ID_HEADER.to_string(),
            "request-123".to_string(),
        );
        inbound.insert(BETA_FEATURES_HEADER.to_string(), "js_repl".to_string());
        decorate_codex_http_request(
            &mut http_req,
            "access-token",
            false,
            Some(&inbound),
            &prepared,
            None,
            "generated-session",
        );
        assert_eq!(
            http_req
                .headers
                .get(TURN_METADATA_HEADER)
                .map(String::as_str),
            Some(r#"{"session_id":"turn-session","turn_id":"turn-123"}"#)
        );
        assert_eq!(
            http_req.headers.get(WINDOW_ID_HEADER).map(String::as_str),
            Some("window-123")
        );
        assert_eq!(
            http_req
                .headers
                .get(CLIENT_REQUEST_ID_HEADER)
                .map(String::as_str),
            Some("request-123")
        );
        assert_eq!(
            http_req
                .headers
                .get(BETA_FEATURES_HEADER)
                .map(String::as_str),
            Some("js_repl")
        );
        Ok(())
    }

    /// Mirrors Go `TestCodexOutbound_SessionIDPrecedence` — five-level
    /// waterfall: header > turn-metadata > existing-on-request >
    /// context > generated.
    #[test]
    fn decorate_session_id_precedence_inbound_header_wins() -> TestResult {
        let mut http_req = make_bare_http_request();
        http_req
            .headers
            .insert(SESSION_HEADER.to_string(), "existing".to_string());
        let prepared = make_prepared("header-session", "", "", "", "");
        decorate_codex_http_request(
            &mut http_req,
            "tok",
            false,
            None,
            &prepared,
            Some("context-session"),
            "generated",
        );
        assert_eq!(
            http_req.headers.get(SESSION_HEADER).map(String::as_str),
            Some("header-session")
        );
        Ok(())
    }

    #[test]
    fn decorate_session_id_precedence_turn_metadata_fallback() -> TestResult {
        let mut http_req = make_bare_http_request();
        let prepared = make_prepared("", "", "", r#"{"session_id":"turn-session"}"#, "");
        decorate_codex_http_request(
            &mut http_req,
            "tok",
            false,
            None,
            &prepared,
            Some("context-session"),
            "generated",
        );
        assert_eq!(
            http_req.headers.get(SESSION_HEADER).map(String::as_str),
            Some("turn-session")
        );
        Ok(())
    }

    #[test]
    fn decorate_session_id_precedence_existing_on_request() -> TestResult {
        let mut http_req = make_bare_http_request();
        http_req
            .headers
            .insert(SESSION_HEADER.to_string(), "existing".to_string());
        let prepared = make_prepared("", "", "", "", "");
        decorate_codex_http_request(
            &mut http_req,
            "tok",
            false,
            None,
            &prepared,
            Some("context-session"),
            "generated",
        );
        assert_eq!(
            http_req.headers.get(SESSION_HEADER).map(String::as_str),
            Some("existing")
        );
        Ok(())
    }

    #[test]
    fn decorate_session_id_precedence_context_then_generated() -> TestResult {
        // Context session wins over generated.
        let mut http_req = make_bare_http_request();
        let prepared = make_prepared("", "", "", "", "");
        decorate_codex_http_request(
            &mut http_req,
            "tok",
            false,
            None,
            &prepared,
            Some("context-session"),
            "generated",
        );
        assert_eq!(
            http_req.headers.get(SESSION_HEADER).map(String::as_str),
            Some("context-session")
        );

        // No context → generated uuid is the final fallback.
        let mut http_req = make_bare_http_request();
        decorate_codex_http_request(
            &mut http_req,
            "tok",
            false,
            None,
            &prepared,
            None,
            "uuid-1234",
        );
        assert_eq!(
            http_req.headers.get(SESSION_HEADER).map(String::as_str),
            Some("uuid-1234")
        );
        Ok(())
    }

    /// Mirrors Go `TestCodexOutbound_StreamAcceptHeader` — compact requests
    /// get `application/json`, everything else gets `text/event-stream`.
    #[test]
    fn decorate_accept_header_compact_vs_stream() -> TestResult {
        let prepared = make_prepared("", "", "", "", "");
        // Stream (non-compact).
        let mut http_req = make_bare_http_request();
        decorate_codex_http_request(&mut http_req, "tok", false, None, &prepared, None, "g");
        assert_eq!(
            http_req.headers.get("Accept").map(String::as_str),
            Some("text/event-stream")
        );

        // Compact.
        let mut http_req = make_bare_http_request();
        decorate_codex_http_request(&mut http_req, "tok", true, None, &prepared, None, "g");
        assert_eq!(
            http_req.headers.get("Accept").map(String::as_str),
            Some("application/json")
        );
        Ok(())
    }
}

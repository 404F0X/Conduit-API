//! OpenAI outbound transformer — auth header (S06), endpoint URL (S07),
//! usage extraction (S10).
//!
//! Mirrors the outbound side of Go's `conduit/llm/transformer/openai/outbound.go`
//! and `usage.go`. Only the three pure pieces of RUST-P7-002 live here:
//!
//! * [`build_auth_header`] — Go `OutboundTransformer.TransformRequest` header +
//!   `AuthConfig{Type:"bearer", APIKey}` construction (outbound.go:204-213).
//! * [`resolve_outbound_url`] — Go `OutboundTransformer.buildFullRequestURL`
//!   (outbound.go:407-417) layered on `transformer.NormalizeBaseURL` /
//!   `BuildRequestURL` (url.go).
//! * [`extract_usage`] — Go `Usage.ToLLMUsage` (usage.go:34-71), which folds
//!   the OpenAI-compatible `Usage` JSON object into the unified [`Usage`]
//!   (including the Moonshot top-level `cached_tokens` fallback and the
//!   DeepSeek-style `prompt_tokens_details.write_cached_tokens`).
//!
//! These helpers are deliberately kept pure (no I/O, no provider trait) so the
//! full `OutboundTransformer` impl in RUST-P7-002 S04/S08/S09 can compose them
//! later, and so they can be unit-tested directly against the Go golden cases
//! in `outbound_test.go` and `usage_test.go`.

use conduit_core::ConduitError;
use conduit_llm::model::{HeaderMap, ResponsesRequest};
use conduit_llm::{
    ApiFormat, HttpAuth, HttpRequest, LlmRequest, LlmRequestPayload, RequestType, Usage,
};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::TransformerResult;

/// Default OpenAI chat-completions endpoint path appended after the normalized
/// base URL. Mirrors Go `buildFullRequestURL` (outbound.go:416).
pub const DEFAULT_CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// Default API version segment appended to the base URL when the URL doesn't
/// already carry it and no custom endpoint path is set. Mirrors Go
/// `NewOutboundTransformerWithConfig` (outbound.go:106).
pub const DEFAULT_OPENAI_VERSION: &str = "v1";

/// Platform type — mirrors Go `PlatformType` (outbound.go:23-28). Only the two
/// values the Go `validateConfig` switch accepts are modeled (`openai`,
/// `google`); unknown values are rejected by [`Config::validate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlatformType {
    #[default]
    OpenAi,
    Google,
}

impl PlatformType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Google => "google",
        }
    }

    /// Parse a platform type tag, mirroring Go's `validateConfig` switch which
    /// only accepts `"openai"` / `"google"` and rejects everything else.
    pub fn parse(value: &str) -> TransformerResult<Self> {
        match value {
            "openai" | "" => Ok(Self::OpenAi),
            "google" => Ok(Self::Google),
            other => Err(ConduitError::invalid_request(format!(
                "unsupported platform type: {other}"
            ))),
        }
    }
}

/// Outbound transformer configuration — mirrors Go `Config` (outbound.go:45-68).
///
/// `api_key_provider` is replaced by a simple `api_key: String` since this
/// module only models the pure auth-header construction (the
/// `APIKeyProvider.Get(ctx)` indirection adds I/O and is not needed for the
/// pure S06 helper). Downstream code that needs rotating keys can construct
/// the [`Config`] per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Platform type — `openai` (default) or `google`. Drives nothing here
    /// directly (Go's only platform branch is in `TransformRequest` for
    /// stripping tool-call extras) but is validated for parity.
    pub platform_type: PlatformType,
    /// Base URL for the OpenAI-compatible API, e.g.
    /// `https://api.openai.com/v1`. Required.
    pub base_url: String,
    /// When `true`, the request URL is used as-is (no `/chat/completions`
    /// suffix). Mirrors Go `Config.RawURL` (outbound.go:53-54). Auto-enabled
    /// when `base_url` ends with `##`.
    pub raw_url: bool,
    /// Custom endpoint path that overrides the default `/chat/completions`
    /// suffix. Must start with `/`. When set, the default version (`v1`) is
    /// *not* appended to the base URL during normalization. Mirrors Go
    /// `Config.EndpointPath` (outbound.go:58-59).
    pub endpoint_path: Option<String>,
    /// API key used for the outbound `Authorization: Bearer <key>` header.
    /// Required.
    pub api_key: String,
}

impl Config {
    /// Validate the configuration, mirroring Go `validateConfig`
    /// (outbound.go:115-135): non-empty base URL, non-empty API key, and a
    /// supported platform type.
    pub fn validate(&self) -> TransformerResult<()> {
        if self.base_url.is_empty() {
            return Err(ConduitError::invalid_request("base URL is required"));
        }
        if self.api_key.is_empty() {
            return Err(ConduitError::invalid_request("API key is required"));
        }
        // platform_type is already a constrained enum; no further check needed.
        Ok(())
    }
}

/// S06 — Build the OpenAI outbound auth header set + HTTP auth descriptor.
///
/// Mirrors Go `OutboundTransformer.TransformRequest` (outbound.go:204-213):
///
/// ```text
/// headers := make(http.Header)
/// headers.Set("Content-Type", "application/json")
/// headers.Set("Accept", "application/json")
///
/// authConfig := &httpclient.AuthConfig{
///     Type:   "bearer",
///     APIKey: apiKey,
/// }
/// ```
///
/// All OpenAI-compatible providers use the standard `Authorization: Bearer
/// <api_key>` header (Go's `AuthConfig.Type == "bearer"`). The
/// `Authorization` and `Content-Type` / `Accept` headers are written onto the
/// returned [`HeaderMap`]; the bearer token is *also* captured in an [`HttpAuth`]
/// so downstream transport code that reads `request.auth` (rather than the
/// header map) can apply it identically to Go's `httpclient` layer.
///
/// `channel_type` is accepted for parity with the task spec ("provider-specific
/// api-key header variants") and forwarded to [`auth_header_for_channel`]; it
/// is otherwise unused for the default OpenAI bearer scheme.
pub fn build_auth_header(
    api_key: &str,
    channel_type: &str,
) -> TransformerResult<(HeaderMap, HttpAuth)> {
    if api_key.is_empty() {
        return Err(ConduitError::invalid_request("API key is required"));
    }

    let (scheme, token) = auth_header_for_channel(api_key, channel_type);

    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Authorization".to_string(), format!("{scheme} {token}"));

    let auth = HttpAuth {
        // Store the Go `AuthConfig.Type` value (`"bearer"`) verbatim so the
        // transport layer matches Go's `httpclient` contract.
        scheme: "bearer".to_string(),
        token: Some(token),
        ..HttpAuth::default()
    };
    Ok((headers, auth))
}

/// Resolve the per-channel auth scheme. Default OpenAI-compatible providers
/// (and Google's OpenAI-compatible endpoint) use the standard
/// `Authorization: Bearer <api_key>` scheme. Provider-specific variants that
/// require a raw API key header (e.g. DeepSeek/Moonshot which also accept
/// `Authorization: Bearer`, or Google AI Studio which uses `x-goog-api-key`)
/// can be extended here without touching [`build_auth_header`].
///
/// Currently every OpenAI-compatible channel goes through the bearer branch,
/// which matches Go's hardcoded `Type: "bearer"` in
/// `OutboundTransformer.TransformRequest` (outbound.go:210).
fn auth_header_for_channel(api_key: &str, _channel_type: &str) -> (&'static str, String) {
    ("Bearer", api_key.to_string())
}

/// S07 — Resolve the full outbound request URL from a channel's base URL and
/// endpoint configuration.
///
/// Mirrors Go `OutboundTransformer.buildFullRequestURL` (outbound.go:407-417)
/// composed with `transformer.NormalizeBaseURL` (url.go:16-44) and the
/// `##` / `#` auto-detection done in `NewOutboundTransformerWithConfig`
/// (outbound.go:98-107):
///
/// ```text
/// if strings.HasSuffix(config.BaseURL, "##") {
///     config.RawURL = true
///     config.BaseURL = strings.TrimSuffix(config.BaseURL, "##")
/// } else if !config.RawURL {
///     if config.EndpointPath != "" {
///         config.BaseURL = transformer.NormalizeBaseURL(config.BaseURL, "")
///     } else {
///         config.BaseURL = transformer.NormalizeBaseURL(config.BaseURL, "v1")
///     }
/// }
/// ```
///
/// then at request time:
///
/// ```text
/// if t.config.RawURL          { return t.config.BaseURL, nil }
/// if t.config.EndpointPath != "" { return t.config.BaseURL + t.config.EndpointPath, nil }
/// return t.config.BaseURL + "/chat/completions", nil
/// ```
///
/// `base_url` is taken by value because the `##` / `#` markers may rewrite it
/// (mirroring Go mutating `config.BaseURL` in place during construction).
pub fn resolve_outbound_url(
    mut base_url: String,
    endpoint_path: Option<&str>,
    raw_url: bool,
) -> TransformerResult<String> {
    // Go `##` detection: enables raw URL mode and strips the suffix.
    if let Some(stripped) = base_url.strip_suffix("##") {
        return Ok(normalize_base_url(stripped.to_string(), "")
            .trim_end_matches('/')
            .to_string());
    }

    if raw_url {
        // Go: `return t.config.BaseURL, nil` (RawURL branch — no trailing
        // slash trim, used verbatim).
        return Ok(base_url);
    }

    // Normalize the base URL. Version `v1` is appended only when no custom
    // endpoint path is set (Go outbound.go:102-106).
    let version = if endpoint_path.map(str::is_empty).unwrap_or(true) {
        DEFAULT_OPENAI_VERSION
    } else {
        ""
    };
    let normalized = normalize_base_url(base_url.clone(), version);

    if let Some(path) = endpoint_path.filter(|path| !path.is_empty()) {
        // Custom endpoint path override — appended verbatim. The Go side does
        // not require a leading `/`, but `validateConfig`-adjacent docs note it
        // must start with `/`; we accept whatever the channel supplies to stay
        // lossless.
        base_url = format!("{normalized}{path}");
        Ok(base_url)
    } else {
        Ok(format!("{normalized}{DEFAULT_CHAT_COMPLETIONS_PATH}"))
    }
}

// ---------------------------------------------------------------------------
// RUST-P7-002 S11 — Endpoint path override (per-endpoint dispatch).
//
// Go's OpenAI outbound transformer repeats the same override-or-default idiom
// at every per-endpoint URL builder:
//
//   * `buildFullRequestURL` (outbound.go:407-417)         → `/chat/completions`
//   * `buildEmbeddingURL` (embedding.go:96-102)           → `/embeddings`
//   * `buildImageGenerateRequest` (image_outbound.go:155) → `/images/generations`
//   * `buildImageEditRequest` (image_outbound.go:379)     → `/images/edits`
//   * `buildImageVariationRequest` (image_outbound.go:503)→ `/images/variations`
//   * `buildAudioURL("/audio/speech")` (audio_outbound.go:257-264)
//   * `buildAudioURL("/audio/transcriptions")` / `("/audio/translations")`
//   * responses `buildFullRequestURL` (responses/outbound.go:320-331) → `/responses`
//
// Each builder follows the identical shape:
//
//   ```text
//   if t.config.EndpointPath != "" {
//       return t.config.BaseURL + t.config.EndpointPath, nil
//   }
//   return t.config.BaseURL + defaultPath, nil
//   ```
//
// The channel-configured `EndpointPath` therefore overrides the
// per-endpoint default path globally — a channel that sets `EndpointPath =
// "/v2/chat"` rewrites *every* outbound request's path, not just chat. This
// is the S11 invariant the coordinator flagged: the override must apply to
// the outbound URL, not just the base_url.
//
// `resolve_outbound_url` above already implements the chat-completions
// specialization of this rule. [`resolve_outbound_path`] extracts the pure
// path-selection piece so per-endpoint outbound builders (embeddings, audio,
// image, …) can compose it without re-implementing the override check, and
// [`resolve_outbound_url_for_endpoint`] is the per-endpoint URL resolver that
// reproduces the Go builders verbatim.
// ---------------------------------------------------------------------------

/// Default endpoint paths per OpenAI-compatible API surface. Mirrors the
/// hardcoded default-path literals in the Go per-endpoint URL builders
/// referenced above.
pub const DEFAULT_COMPLETIONS_PATH: &str = "/completions";
pub const DEFAULT_EMBEDDINGS_PATH: &str = "/embeddings";
pub const DEFAULT_IMAGES_GENERATIONS_PATH: &str = "/images/generations";
pub const DEFAULT_IMAGES_EDITS_PATH: &str = "/images/edits";
pub const DEFAULT_IMAGES_VARIATIONS_PATH: &str = "/images/variations";
pub const DEFAULT_AUDIO_SPEECH_PATH: &str = "/audio/speech";
pub const DEFAULT_AUDIO_TRANSCRIPTIONS_PATH: &str = "/audio/transcriptions";
pub const DEFAULT_AUDIO_TRANSLATIONS_PATH: &str = "/audio/translations";
pub const DEFAULT_RESPONSES_PATH: &str = "/responses";

/// S11 — Select the outbound path component, honoring a channel-configured
/// `endpoint_path` override.
///
/// Mirrors the override-or-default idiom every Go per-endpoint URL builder
/// applies (outbound.go:412-416, embedding.go:98-102, audio_outbound.go:259-263,
/// image_outbound.go:156-158 / :380-381 / :504-505, responses/outbound.go:326-330):
///
/// ```text
/// if t.config.EndpointPath != "" { return t.config.EndpointPath }
/// return defaultPath
/// ```
///
/// The override is **channel-global**: when set on a channel's config it
/// replaces every per-endpoint default path, not just one. Callers pass the
/// per-endpoint default (e.g. [`DEFAULT_EMBEDDINGS_PATH`] for the embeddings
/// builder) and this helper returns the override verbatim when present, else
/// the default.
///
/// `endpoint_path` is trimmed before the empty-check so a whitespace-only
/// override is treated as unset (Go's `!= ""` check would also see `" "` as
/// set, but no real channel config produces whitespace-only paths; we mirror
/// Go's exact `!= ""` semantics by only trimming for the emptiness test, not
/// for the returned value).
pub fn resolve_outbound_path<'a>(endpoint_path: Option<&'a str>, default_path: &'a str) -> &'a str {
    match endpoint_path.map(str::trim).filter(|path| !path.is_empty()) {
        // Override wins — returned verbatim, mirroring Go's
        // `return t.config.EndpointPath` (no leading-slash enforcement, no
        // normalization; the channel is authoritative).
        Some(path) => path,
        None => default_path,
    }
}

/// S11 — Resolve the full outbound URL for a specific endpoint, applying the
/// channel-configured `endpoint_path` override on top of the per-endpoint
/// default path.
///
/// Composes the base-URL normalization Go performs at construction time
/// (`NewOutboundTransformerWithConfig`, outbound.go:98-107 — identical across
/// endpoints) with [`resolve_outbound_path`]'s override-or-default path
/// selection. This is the direct Rust analogue of Go's per-endpoint builders
/// (`buildEmbeddingURL`, `buildAudioURL(defaultPath)`, the image builders,
/// responses `buildFullRequestURL`), which all share the same base-URL
/// normalization and then apply their per-endpoint default at request time.
///
/// # Go parity
///
/// For every endpoint the Go sequence is:
/// 1. At construction: `NormalizeBaseURL(base, "")` if `EndpointPath != ""`,
///    else `NormalizeBaseURL(base, "v1")` (outbound.go:102-106). When
///    `RawURL` is set (or forced via `##`), the base is used as-is.
/// 2. At request time (outbound.go:407-417, embedding.go:97-102, etc.):
///    `RawURL` → base verbatim; else `EndpointPath != ""` →
///    `base + EndpointPath`; else `base + defaultPath`.
///
/// This helper reproduces both steps for an arbitrary `default_path`. The
/// `RawURL` / `##` short-circuits return the base with no path appended,
/// mirroring Go's RawURL branch which also skips path selection
/// (outbound.go:408-410).
pub fn resolve_outbound_url_for_endpoint(
    base_url: String,
    endpoint_path: Option<&str>,
    raw_url: bool,
    default_path: &str,
) -> TransformerResult<String> {
    // Step 1: `##` short-circuit — strip the marker and return the base
    // verbatim (no version, no path). Mirrors resolve_outbound_url + Go's
    // `##` detection at outbound.go:98-100.
    if let Some(stripped) = base_url.strip_suffix("##") {
        return Ok(normalize_base_url(stripped.to_string(), "")
            .trim_end_matches('/')
            .to_string());
    }

    // Step 1 (alt): RawURL short-circuit — base used as-is, no path append
    // (Go outbound.go:408-410).
    if raw_url {
        return Ok(base_url);
    }

    // Step 1 (cont.): version selection — `v1` is appended only when no
    // override path is configured (Go outbound.go:102-106).
    let has_override = endpoint_path
        .map(str::trim)
        .map_or(false, |path| !path.is_empty());
    let version = if has_override {
        ""
    } else {
        DEFAULT_OPENAI_VERSION
    };
    let normalized = normalize_base_url(base_url, version);

    // Step 2: path selection — override wins, else per-endpoint default
    // (Go's per-endpoint builders, e.g. embedding.go:98-102).
    let path = resolve_outbound_path(endpoint_path, default_path);
    Ok(format!("{normalized}{path}"))
}

// ---------------------------------------------------------------------------
// RUST-P7-002 S11 (cont.) — model mapping / override headers / base_url.
//
// In the Go architecture, these three channel-config concerns are applied by
// **orchestrator-layer middleware that runs *before* the OpenAI outbound
// transformer** (see `internal/server/orchestrator/`):
//
//   * model mapping  — `model_mapper.go::applyModelMapping` rewrites
//     `llmReq.Model` from the API-key profile's `ModelMappings` list; the
//     transformer simply reads `llmReq.Model`.
//   * override headers — `override.go::applyOverrideRequestHeaders` mutates
//     the `httpclient.Request.Headers` via per-op `OverrideOperation`s
//     (`set` / `delete` / `rename` / `copy`, plus the `__CONDUIT_CLEAR__`
//     sentinel); the transformer reads the already-mutated header map.
//   * base_url — set on `Config.BaseURL` from the channel record before the
//     transformer is constructed; the transformer consumes it verbatim
//     (modulo `NormalizeBaseURL`).
//
// The helpers below capture the *pure* logic of each of those pre-transformer
// concerns so that future transformer-wiring code (or a Rust port of the
// orchestrator middleware) can compose them without re-deriving the rules.
// They are deliberately side-effect-free — no I/O, no global caches — so they
// can be unit-tested directly against the Go golden cases in
// `model_mapper_test.go` and `override_test.go`.
// ---------------------------------------------------------------------------

/// Single model-mapping rule. Mirrors Go `objects.ModelMapping`
/// (internal/objects/channel.go:33-39): `from` is matched against the inbound
/// model id; `to` is the provider-side model id substituted on a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMapping {
    pub from: String,
    pub to: String,
}

impl ModelMapping {
    /// Construct a `from → to` mapping rule.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// Apply an ordered list of channel model mappings to an inbound model id,
/// mirroring Go `ModelMapper.applyModelMapping`
/// (internal/server/orchestrator/model_mapper.go:148-158).
///
/// Iterates mappings in order and returns the first matching rule's `to`
/// value; if no rule matches, the original model id is returned unchanged.
/// Match semantics are Go `xregexp.MatchString`
/// (internal/pkg/xregexp/match.go:21-39):
///
/// * `from == "*"` → matches every model (the wildcard short-circuit).
/// * `from` contains no regex metacharacters (`* ? + [ ] { } ( ) ^ $ . | \`)
///   → exact string equality (`from == model`).
/// * otherwise → `from` is treated as an **anchored regex**
///   (`^(?:<body>)$`, with outer `^`/`$` stripped and inline `(?<modifiers>)`
///   prefixes preserved). Invalid regex patterns never match (parity with
///   Go's `compileErr` short-circuit at match.go:24-26).
///
/// # Go parity
///
/// `applyModelMapping` returns `model` unchanged when no rule fires
/// (model_mapper.go:157); we reproduce that fallthrough here. The first
/// matching rule wins (Go iterates and returns on first match, line 152-155).
pub fn map_model<'a>(mappings: &'a [ModelMapping], model: &str) -> String {
    for mapping in mappings {
        if model_pattern_matches(&mapping.from, model) {
            return mapping.to.clone();
        }
    }
    model.to_string()
}

/// Go `xregexp.MatchString` parity (internal/pkg/xregexp/match.go:21-39 +
/// `getOrCreatePattern` at :73-104). See [`map_model`] for the case analysis.
fn model_pattern_matches(pattern: &str, model: &str) -> bool {
    // Wildcard short-circuit (match.go:80-85).
    if pattern == "*" {
        return true;
    }

    // No regex chars → exact equality (match.go:87-92).
    if !contains_regex_chars(pattern) {
        return pattern == model;
    }

    // Regex path. Go compiles with `ensureAnchored` (match.go:106-112) which
    // (a) peels a leading inline modifier `(?<flags>)`, (b) strips outer
    // `^`/`$`, (c) re-anchors as `^(?:<body>)$`. We reproduce that transform
    // and then compile with Rust's `regex` crate. Compile failure → no match
    // (match.go:24-26 `compileErr` short-circuit).
    let (modifier, body) = split_inline_modifier(pattern);
    let stripped = body
        .strip_prefix('^')
        .unwrap_or(body)
        .strip_suffix('$')
        .unwrap_or(body); // unwrap_or is safe: strip_prefix returns Option
    let anchored = format!("{modifier}^(?:{stripped})$");
    match regex::Regex::new(&anchored) {
        Ok(re) => re.is_match(model),
        Err(_) => false,
    }
}

/// Go `containsRegexChars` (internal/pkg/xregexp/match.go:127-129).
fn contains_regex_chars(pattern: &str) -> bool {
    pattern.chars().any(|c| {
        matches!(
            c,
            '*' | '?' | '+' | '[' | ']' | '{' | '}' | '(' | ')' | '^' | '$' | '.' | '|' | '\\'
        )
    })
}

/// Go `splitInlineModifier` (internal/pkg/xregexp/match.go:131-…): peel a
/// leading `(?<flags>)` inline modifier prefix off `pattern`, returning
/// `(modifier, body)`. If `pattern` does not start with `(?`, returns
/// `("", pattern)`.
fn split_inline_modifier(pattern: &str) -> (&str, &str) {
    if !pattern.starts_with("(?") {
        return ("", pattern);
    }
    match pattern.find(')') {
        Some(end) if end > 2 => (&pattern[..=end], &pattern[end + 1..]),
        _ => ("", pattern),
    }
}

/// S11 — Resolve the outbound `base_url` from a channel's configured base URL.
///
/// This is a deliberately thin helper that documents the boundary contract:
/// the orchestrator sets the channel's `base_url` onto the transformer's
/// [`Config`] before construction, and the transformer consumes it via
/// [`resolve_outbound_url`] / [`resolve_outbound_url_for_endpoint`] (which
/// apply `NormalizeBaseURL`, `##` / `#` / `RawURL` handling). The helper:
///
/// * rejects an empty base URL with the Go `validateConfig` error string
///   ("base URL is required") so callers fail fast at the boundary
///   (completion_outbound.go:30-32 / outbound.go `validateConfig` :122-124);
/// * returns the base URL verbatim otherwise — normalization happens in
///   `resolve_outbound_url*`, not here, mirroring Go's separation of
///   concerns (`validateConfig` checks non-empty; `NormalizeBaseURL` runs at
///   `NewOutboundTransformerWithConfig` time).
///
/// Returning a `Result` lets future wiring chain this with
/// [`resolve_outbound_url_for_endpoint`] without an extra validation step.
pub fn apply_outbound_base_url(channel_base_url: &str) -> TransformerResult<String> {
    if channel_base_url.trim().is_empty() {
        return Err(ConduitError::invalid_request("base URL is required"));
    }
    Ok(channel_base_url.to_string())
}

/// S11 — Apply a single override-header operation to a header map in place,
/// mirroring Go `applyOverrideOperationToHeaders`
/// (internal/server/orchestrator/override.go:394-437).
///
/// Supported ops (objects/channel.go:48-50, OverrideOperation.Op):
///
/// * `"set"`   — render `value` and `headers.set(path, value)`; if the
///   rendered value equals `__CONDUIT_CLEAR__`, delete the header instead
///   (override.go:405-413).
/// * `"delete"`— `headers.del(path)` (override.go:414-415).
/// * `"rename"`— move all values at `from` to `to`
///   (override.go:416-426): if `from` is absent this is a no-op, else
///   `del(from)` then `add(to, value)` for each value.
/// * `"copy"`  — for each value at `from`, `add(to, value)`
///   (override.go:427-431): unlike rename, `from` is preserved.
///
/// Template rendering and condition evaluation are **not** handled here —
/// the Go middleware renders templates against a `RenderContext`
/// (`request_model`, `model`, `metadata`, …) before invoking this logic.
/// Callers responsible for the orchestration layer should pre-render
/// `value`/`condition` and only call this helper when the condition is
/// satisfied. The pure header-map mutation is what this helper owns.
///
/// `HeaderMap` (a `BTreeMap<String, String>`) cannot carry duplicate values
/// for the same key the way Go's `http.Header` (`map[string][]string`) does;
/// for `set`/`rename`/`copy` we follow Rust's first-write semantics
/// (`BTreeMap::insert` overwrites). This matches the effective behavior for
/// the single-value header case (the common one) — multi-valued header
/// overrides are an orchestrator-layer concern handled before the
/// transformer sees the request.
pub fn apply_override_header_op(
    headers: &mut HeaderMap,
    op: &OverrideHeaderOp,
    rendered_value: &str,
) {
    match op.kind {
        OverrideHeaderKind::Set => {
            // `__CONDUIT_CLEAR__` sentinel → delete (override.go:408-411).
            if rendered_value == CONDUIT_CLEAR_SENTINEL {
                headers.remove(&op.path);
            } else {
                headers.insert(op.path.clone(), rendered_value.to_string());
            }
        }
        OverrideHeaderKind::Delete => {
            headers.remove(&op.path);
        }
        OverrideHeaderKind::Rename => {
            // Move `from` → `to`. No-op if `from` is absent
            // (override.go:417-419).
            if let Some(value) = headers.remove(&op.from) {
                headers.insert(op.to.clone(), value);
            }
        }
        OverrideHeaderKind::Copy => {
            // Copy `from` → `to` without removing `from`. No-op if absent
            // (override.go:427-430). On the BTreeMap model this is "insert
            // only if `to` is not already present" so we don't clobber a
            // pre-existing value at `to` with a stale copy — parity with
            // Go's `headers.Add` appending behavior on multi-valued headers
            // (where the existing `to` values survive).
            if let Some(value) = headers.get(&op.from).cloned() {
                headers.entry(op.to.clone()).or_insert(value);
            }
        }
    }
}

/// Sentinel value Go uses to clear a header on the `set` op
/// (override.go:226, :408-411). Kept as a constant so callers and tests
/// reference the canonical literal.
pub const CONDUIT_CLEAR_SENTINEL: &str = "__CONDUIT_CLEAR__";

/// Kinds of override-header operations. Mirrors Go `OverrideOp*` constants
/// for the header-applicable subset (objects/channel.go:48-50).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideHeaderKind {
    Set,
    Delete,
    Rename,
    Copy,
}

/// Header-applicable slice of Go `OverrideOperation`
/// (objects/channel.go:67-86). Only the fields [`apply_override_header_op`]
/// reads are modeled; the orchestrator-layer `condition` / `value` template
/// rendering is performed before constructing this pure record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideHeaderOp {
    pub kind: OverrideHeaderKind,
    /// `path` is the header name for `set`/`delete` (`op.Path`).
    pub path: String,
    /// `from` is the source header name for `rename`/`copy` (`op.From`).
    pub from: String,
    /// `to` is the destination header name for `rename`/`copy` (`op.To`).
    pub to: String,
}

impl OverrideHeaderOp {
    /// Build a `set` op (Go `OverrideOpSet`).
    pub fn set(path: impl Into<String>) -> Self {
        Self {
            kind: OverrideHeaderKind::Set,
            path: path.into(),
            from: String::new(),
            to: String::new(),
        }
    }
    /// Build a `delete` op (Go `OverrideOpDelete`).
    pub fn delete(path: impl Into<String>) -> Self {
        Self {
            kind: OverrideHeaderKind::Delete,
            path: path.into(),
            from: String::new(),
            to: String::new(),
        }
    }
    /// Build a `rename` op (Go `OverrideOpRename`).
    pub fn rename(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind: OverrideHeaderKind::Rename,
            path: String::new(),
            from: from.into(),
            to: to.into(),
        }
    }
    /// Build a `copy` op (Go `OverrideOpCopy`).
    pub fn copy(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            kind: OverrideHeaderKind::Copy,
            path: String::new(),
            from: from.into(),
            to: to.into(),
        }
    }
}

/// Port of Go `transformer.NormalizeBaseURL` (url.go:16-44).
///
/// Rules (in Go order):
/// 1. empty in → empty out
/// 2. trailing `#` strips the marker and skips version appending
/// 3. empty `version` → just trim trailing slashes
/// 4. URL already ending in `/<version>` → trim trailing slashes
/// 5. URL containing `/<version>/` → trim trailing slashes
/// 6. otherwise → trim trailing slashes then append `/<version>`
///
/// Exposed publicly so the shared OpenAI-compatible base (`openai_compatible`)
/// and the thin provider wrappers (deepseek/moonshot/...) can compose it the
/// same way the Go wrappers call `transformer.NormalizeBaseURL`.
pub fn normalize_base_url(url: String, version: &str) -> String {
    if url.is_empty() {
        return String::new();
    }

    // Rule 2: trailing `#` marker — strip and skip version.
    if let Some(before) = url.strip_suffix('#') {
        return trim_trailing_slashes(before);
    }

    if version.is_empty() {
        return trim_trailing_slashes(&url);
    }

    let version_segment = format!("/{version}");

    // Rule 4: URL already ends with `/<version>`.
    if url.ends_with(&version_segment) {
        return trim_trailing_slashes(&url);
    }

    // Rule 5: `/<version>/` appears mid-path.
    if url.contains(&format!("{version_segment}/")) {
        return trim_trailing_slashes(&url);
    }

    // Rule 6: append the version.
    let trimmed = trim_trailing_slashes(&url);
    format!("{trimmed}/{version}")
}

/// Trim every trailing `/` (Go uses `strings.TrimRight(url, "/")`). Unlike
/// `str::trim_end_matches('/')`, `TrimRight` removes *all* trailing slashes,
/// which `trim_end_matches` also does — but we keep a named helper for parity
/// with the Go source.
fn trim_trailing_slashes(input: &str) -> String {
    input.trim_end_matches('/').to_string()
}

/// S10 — Extract the unified [`Usage`] from an OpenAI-compatible provider
/// response JSON.
///
/// Mirrors Go `Usage.ToLLMUsage` (usage.go:34-71). The input is the
/// *response body JSON* (the OpenAI `Usage` object); the function reads:
///
/// * `prompt_tokens` / `completion_tokens` / `total_tokens` → top-level fields
/// * `prompt_tokens_details.{audio_tokens,cached_tokens,write_cached_tokens}`
///   → `Usage.prompt_details`
/// * `completion_tokens_details.{audio_tokens,reasoning_tokens,
///   accepted_prediction_tokens,rejected_prediction_tokens}` →
///   `Usage.completion_details`
/// * top-level `cached_tokens` (Moonshot style) → folded into
///   `prompt_details.cached_tokens` *only when* the structured details don't
///   already carry a non-zero `cached_tokens`
///
/// Any additional provider fields are preserved losslessly through the unified
/// [`Usage`]'s `extra` flatten.
///
/// Returns `None` when `response_json` has no `usage` object, matching the Go
/// behavior where a response without `usage` simply yields a nil `*llm.Usage`.
pub fn extract_usage(response_json: &Value) -> Option<Usage> {
    let usage = response_json.get("usage")?;
    let openai_usage: OpenAiUsage = serde_json::from_value(usage.clone()).ok()?;
    Some(openai_usage.to_unified())
}

/// OpenAI-compatible usage JSON shape mirroring Go's `Usage` /
/// `PromptTokensDetails` / `CompletionTokensDetails` (usage.go:6-32). Used
/// internally by [`extract_usage`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct OpenAiUsage {
    #[serde(default, alias = "input_tokens")]
    prompt_tokens: u64,
    #[serde(default, alias = "output_tokens")]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default, alias = "input_tokens_details")]
    prompt_tokens_details: PromptTokensDetails,
    #[serde(default, alias = "output_tokens_details")]
    completion_tokens_details: CompletionTokensDetails,
    /// Moonshot-style top-level cached tokens (usage.go:30-31). Folded into
    /// `prompt_details.cached_tokens` by [`OpenAiUsage::to_unified`] when the
    /// structured details don't already carry a value.
    #[serde(default)]
    cached_tokens: u64,
    /// Keep any other provider-specific usage fields (e.g. `cost`,
    /// `cache_creation_input_tokens`, `provider_cost`) losslessly.
    #[serde(default, flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct PromptTokensDetails {
    #[serde(default)]
    audio_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
    /// `omitempty` in Go (usage.go:10) — hidden internal field used only for
    /// cost calculation but still parsed when present.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    write_cached_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
struct CompletionTokensDetails {
    #[serde(default)]
    audio_tokens: u64,
    #[serde(default)]
    reasoning_tokens: u64,
    #[serde(default)]
    accepted_prediction_tokens: u64,
    #[serde(default)]
    rejected_prediction_tokens: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

impl OpenAiUsage {
    /// Convert to the unified [`Usage`], mirroring Go `Usage.ToLLMUsage`
    /// (usage.go:34-71). See [`extract_usage`] for the field mapping.
    fn to_unified(self) -> Usage {
        let mut usage = Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            total_tokens: self.total_tokens,
            ..Usage::default()
        };

        // Mirror Go's zero-value check (usage.go:45):
        // `if u.PromptTokensDetails != (PromptTokensDetails{})` — i.e. only
        // populate the unified details when the OpenAI details object carries
        // any non-default field.
        if self.prompt_tokens_details != PromptTokensDetails::default() {
            usage.prompt_details.audio_tokens = self.prompt_tokens_details.audio_tokens;
            usage.prompt_details.cached_tokens = self.prompt_tokens_details.cached_tokens;
            usage.prompt_details.write_cached_tokens =
                self.prompt_tokens_details.write_cached_tokens;
        }

        if self.completion_tokens_details != CompletionTokensDetails::default() {
            usage.completion_details.audio_tokens = self.completion_tokens_details.audio_tokens;
            usage.completion_details.reasoning_tokens =
                self.completion_tokens_details.reasoning_tokens;
            usage.completion_details.accepted_prediction_tokens =
                self.completion_tokens_details.accepted_prediction_tokens;
            usage.completion_details.rejected_prediction_tokens =
                self.completion_tokens_details.rejected_prediction_tokens;
        }

        // Moonshot fallback (usage.go:62-68): only when the structured details
        // are absent *or* carry zero cached tokens, and the top-level
        // `cached_tokens` is positive, fold it into prompt_details.cached_tokens.
        if (usage.prompt_details.cached_tokens == 0) && self.cached_tokens > 0 {
            usage.prompt_details.cached_tokens = self.cached_tokens;
        }

        // Preserve unrecognized provider fields losslessly.
        if !self.extra.is_empty() {
            usage
                .extra
                .extend(self.extra.into_iter().map(|(k, v)| (k, v)));
        }

        usage
    }
}

/// Convenience: apply S06 + S07 onto an [`HttpRequest`] builder. Sets method,
/// url, headers, and auth on the request. Mirrors the tail of Go
/// `OutboundTransformer.TransformRequest` (outbound.go:220-228).
///
/// The caller is still responsible for the body and `api_format` fields.
pub fn apply_outbound_transport(
    mut request: HttpRequest,
    config: &Config,
    channel_type: &str,
) -> TransformerResult<HttpRequest> {
    config.validate()?;
    let (headers, auth) = build_auth_header(&config.api_key, channel_type)?;
    let url = resolve_outbound_url(
        config.base_url.clone(),
        config.endpoint_path.as_deref(),
        config.raw_url,
    )?;

    request.method = "POST".to_string();
    request.url = Some(url);
    request.headers.extend(headers);
    request.auth = Some(auth);
    Ok(request)
}

/// S04 — Build the OpenAI-compatible outbound request body (as a JSON
/// [`Value`]) from a unified [`LlmRequest`].
///
/// Mirrors the body-construction half of Go's
/// `OutboundTransformer.TransformRequest` (outbound.go:142-228) and its
/// per-endpoint builders (`transformEmbeddingRequest` in embedding.go,
/// `buildImageGenerateRequest` in image_outbound.go,
/// `buildVideoGenerationAPIRequest` in video_outbound.go,
/// `buildSpeechRequest` / `buildTranscriptionRequest` /
/// `buildTranslationRequest` in audio_outbound.go). The auth header, URL, and
/// transport wiring are handled separately by [`build_auth_header`] /
/// [`resolve_outbound_url`] / [`apply_outbound_transport`]; this helper owns
/// only the JSON body.
///
/// # Dispatch
///
/// The function dispatches on [`LlmRequestPayload`] (the unified payload
/// variant), reproducing Go's `switch llmReq.RequestType` routing:
///
/// | Payload variant           | Go builder                          | OpenAI endpoint                 |
/// |---------------------------|-------------------------------------|---------------------------------|
/// | `Chat` / `Completion`     | `RequestFromLLM` (outbound_convert) | `/v1/chat/completions`          |
/// | `Responses`               | (RUST-P7-002 S05 — not handled here)| `/v1/responses`                 |
/// | `Embedding`               | `transformEmbeddingRequest`         | `/v1/embeddings`                |
/// | `Image`                   | `buildImageGenerateRequest`         | `/v1/images/generations`        |
/// | `Video`                   | `buildVideoGenerationAPIRequest`    | `/v1/videos`                    |
/// | `Audio` (Speech/STT)      | `buildSpeechRequest` / etc.         | `/v1/audio/{speech,transcriptions,translations}` |
///
/// `model` and `stream` live on the unified [`LlmRequest`] (not inside the
/// payload), so each branch injects them onto the serialized body to match
/// Go's flat OpenAI request shape (Go's `RequestFromLLM` reads them off the
/// top-level `llm.Request` and writes them onto the typed `Request{}`).
///
/// # Errors
///
/// Returns [`ConduitError::invalid_request`] when:
/// * `llm_request.model` is `None` or empty (Go: `"model is required"`).
/// * The payload variant is not supported by this OpenAI outbound helper
///   (Go rejects `RequestTypeCompact` / `RequestTypeRerank` with a parity-
///   style "not supported" error; Responses outbound is S05).
pub fn build_openai_outbound_body(llm_request: &LlmRequest) -> TransformerResult<Value> {
    let model = llm_request.model.as_deref().unwrap_or("");
    if model.is_empty() {
        // Mirrors Go `OutboundTransformer.TransformRequest` guard
        // (outbound.go:148-150): "model is required".
        return Err(ConduitError::invalid_request("model is required"));
    }

    // S05 — Compact request-type guard, mirroring Go's standard OpenAI
    // `OutboundTransformer.TransformRequest` switch (outbound.go:166-167):
    // the compact request type is *only* legal on the Responses API path
    // (handled by `LlmRequestPayload::Responses` with `payload.compact ==
    // true` below). Any other payload variant carrying `RequestType::Compact`
    // is rejected with Go's exact error string so non-Responses providers
    // surface the same 400 the Go gateway returns.
    if llm_request.request_type == RequestType::Compact
        && !matches!(
            &llm_request.payload,
            LlmRequestPayload::Responses(payload) if payload.compact
        )
    {
        return Err(ConduitError::invalid_request(
            "compact is only supported by OpenAI Responses API",
        ));
    }

    let mut body = match &llm_request.payload {
        LlmRequestPayload::Chat(payload) => {
            serialize_payload_with(payload, model, llm_request.stream)?
        }
        LlmRequestPayload::Completion(payload) => {
            serialize_payload_with(payload, model, llm_request.stream)?
        }
        LlmRequestPayload::Embedding(payload) => serialize_payload_with(payload, model, false)?,
        LlmRequestPayload::Image(payload) => serialize_payload_with(payload, model, false)?,
        LlmRequestPayload::Video(payload) => serialize_payload_with(payload, model, false)?,
        LlmRequestPayload::Responses(payload) => {
            // S05 — OpenAI Responses outbound. The Responses API has two
            // shapes, dispatched by the payload's `compact` flag (mirroring
            // Go's `OutboundTransformer.TransformRequest` Responses-side
            // switch on `llmReq.RequestType == Compact` in
            // `responses/outbound.go:197-211`):
            //   * compact  → `responses.transformCompactRequest` builds a
            //                `CompactAPIRequest{model, input, instructions,
            //                prompt_cache_key}` (a minimal subset — no tools,
            //                no stream, no reasoning, …).
            //   * standard → `responses.TransformRequest` builds the full
            //                `Request{model, input, instructions, tools,
            //                stream, …}` shape.
            // We reproduce both by serializing the unified `ResponsesRequest`
            // and (for compact) stripping the fields the compact API does
            // not accept, matching Go's compact payload struct.
            build_responses_outbound_body(payload, model, llm_request.stream)?
        }
        LlmRequestPayload::Audio(payload) => match llm_request.request_type {
            // Speech is a JSON-body endpoint; transcription/translation are
            // multipart upstream, but the JSON view the outbound helper
            // produces mirrors Go's `buildAudioMultipartRequest` JSON fields
            // and is what gets persisted to the trace/log. `stream` stays
            // false for every audio kind (Go: `lo.ToPtr(false)`).
            RequestType::Speech => serialize_payload_with(payload, model, false)?,
            RequestType::Transcription | RequestType::Translation => {
                serialize_payload_with(payload, model, false)?
            }
            // Other request types routed to the Audio payload slot are
            // unexpected; surface a parity-style error.
            other => {
                return Err(ConduitError::invalid_request(format!(
                    "unsupported audio request type for OpenAI outbound: {}",
                    other.as_str()
                )));
            }
        },
        LlmRequestPayload::Rerank(_) => Err(ConduitError::invalid_request(
            // Mirrors Go outbound.go:168-169: "rerank is not supported".
            "rerank is not supported by the OpenAI outbound transformer",
        ))?,
        // `LlmRequestPayload` is `#[non_exhaustive]`; future variants added
        // by other P7 tasks (e.g. new provider payload kinds) surface a
        // clear parity-style error rather than silently producing a
        // wrong-shaped body.
        _ => {
            return Err(ConduitError::invalid_request(format!(
                "unsupported payload variant for OpenAI outbound: {}",
                llm_request.payload.request_type()
            )));
        }
    };

    // Inject top-level `model` (always) and `stream` (for chat/completion).
    // Embedding/Image/Video/Audio endpoints never carry a top-level `stream`
    // field in Go's outbound body, so we only inject it for the chat-like
    // payloads. `serialize_payload_with` already wrote `model` + `stream`
    // onto the body for every variant above, but we keep this hook to allow
    // future top-level overrides (e.g. extra_body merge) without re-reading
    // the payload.
    if let Value::Object(ref mut map) = body {
        // Merge `extra_body` (Go forwards provider overrides from
        // `llm.Request.ExtraBody` onto the outbound JSON body).
        merge_extension_map(map, &llm_request.extra_body);
    }

    Ok(body)
}

/// Serialize a typed payload to a JSON [`Value`] and inject the request-level
/// `model` and `stream` fields, mirroring Go's `RequestFromLLM` /
/// `transformEmbeddingRequest` / `buildImageGenerateRequest` which all read
/// `model`/`stream` off the top-level `llm.Request` and write them onto the
/// typed OpenAI request struct before marshaling.
///
/// The `stream` flag is injected unconditionally; callers that want it
/// suppressed for a non-streaming endpoint (embedding/image/video/audio) pass
/// `false`, which matches Go's hardcoded `lo.ToPtr(false)` for those
/// endpoints.
fn serialize_payload_with<T: serde::Serialize>(
    payload: &T,
    model: &str,
    stream: bool,
) -> TransformerResult<Value> {
    let mut value = serde_json::to_value(payload).map_err(|err| {
        ConduitError::internal("failed to serialize OpenAI outbound payload").with_source(err)
    })?;

    if let Value::Object(map) = &mut value {
        // First-write-wins: an explicit `model`/`stream` already on the
        // payload (e.g. a provider extension) is authoritative. The Go side
        // does the equivalent by writing the top-level `Model`/`Stream` onto
        // the typed struct field, which overrides any marshaled value.
        map.entry("model".to_string())
            .or_insert_with(|| Value::String(model.to_string()));
        map.entry("stream".to_string())
            .or_insert(Value::Bool(stream));
    }

    Ok(value)
}

/// Merge `extra` into `dst` without overwriting any existing entry (first
/// write wins, mirroring Go's `ExtraBody` merge semantics where explicit
/// typed fields are authoritative over the extension bag).
fn merge_extension_map(dst: &mut Map<String, Value>, extra: &BTreeMap<String, Value>) {
    for (key, value) in extra {
        dst.entry(key.clone()).or_insert_with(|| value.clone());
    }
}

/// S05 — Build the OpenAI Responses outbound request body.
///
/// Mirrors Go's two Responses-side outbound builders
/// (`responses/outbound.go::TransformRequest` for the standard shape and
/// `responses/compact_outbound.go::transformCompactRequest` for the compact
/// shape), dispatched by the payload's `compact` flag.
///
/// # Standard (non-compact) shape
///
/// The standard Responses body is the typed `Request{}` struct built by Go's
/// `responses.TransformRequest` (outbound.go:247-272). The Rust unified
/// `ResponsesRequest` mirrors that shape directly (`input`, `instructions`,
/// `previous_response_id`, `reasoning`, `tools`, `response_format`, plus
/// `extra` for `parallel_tool_calls` / `stream` / `store` / `service_tier` /
/// `safety_identifier` / `user` / `metadata` / `max_output_tokens` / `top_p`
/// / `tool_choice` / `stream_options` / `prompt_cache_key` / `include` /
/// `max_tool_calls` / `prompt_cache_retention` / `truncation` / …). We
/// serialize it and inject `model` + `stream` from the top-level
/// [`LlmRequest`] fields, matching Go's top-level-field copy onto the typed
/// `Request{}`.
///
/// # Compact shape
///
/// The compact Responses body is the minimal `CompactAPIRequest{}` struct
/// (compact.go:9-16) carrying only `model`, `input`, `instructions`, and
/// `prompt_cache_key`. Go explicitly drops every other field (`tools`,
/// `stream`, `reasoning`, `response_format`, all provider extensions) when
/// building the compact payload — we reproduce that by serializing only those
/// four fields. The `prompt_cache_key` is sourced from the payload's `extra`
/// bag (it has no first-class slot on the Rust unified `ResponsesRequest`),
/// mirroring Go reading it off `llmReq.PromptCacheKey`.
fn build_responses_outbound_body(
    payload: &ResponsesRequest,
    model: &str,
    stream: bool,
) -> TransformerResult<Value> {
    if payload.compact {
        // Go: `responses.CompactAPIRequest{Model, Input, Instructions,
        // PromptCacheKey}`. Only these four fields are forwarded; everything
        // else (tools/stream/reasoning/response_format/extras) is dropped.
        let mut compact = Map::new();
        compact.insert("model".to_string(), Value::String(model.to_string()));
        if let Some(input) = &payload.input {
            compact.insert("input".to_string(), input.clone());
        }
        if let Some(instructions) = payload.instructions.as_ref().filter(|s| !s.is_empty()) {
            compact.insert(
                "instructions".to_string(),
                Value::String(instructions.clone()),
            );
        }
        // `prompt_cache_key` rides via `extra` on the Rust unified
        // `ResponsesRequest` (no first-class slot); preserve it when present
        // and non-empty, matching Go's `omitempty` on `PromptCacheKey`.
        if let Some(prompt_cache_key) = payload
            .extra
            .get("prompt_cache_key")
            .filter(|v| !matches!(v, Value::Null))
        {
            compact.insert("prompt_cache_key".to_string(), prompt_cache_key.clone());
        }
        return Ok(Value::Object(compact));
    }

    // Standard Responses shape: serialize the typed payload and inject
    // `model` + `stream`. The payload already carries `input` /
    // `instructions` / `previous_response_id` / `reasoning` / `tools` /
    // `response_format` as first-class slots and every other field via
    // `extra` flatten (including `stream`, `parallel_tool_calls`,
    // `prompt_cache_key`, …), matching Go's typed `Request{}` field set.
    let mut body = serialize_payload_with(payload, model, stream)?;

    // First-write-wins: an explicit `model`/`stream` already on the payload
    // (e.g. set via `extra`) is authoritative; `serialize_payload_with`
    // already applied that policy. The compact flag is an inbound-routing
    // cue, not a field on the outbound Responses body — drop it so the body
    // matches Go's `Request{}` shape (which has no `compact` field).
    if let Value::Object(map) = &mut body {
        map.remove("compact");
    }

    Ok(body)
}

// ---------------------------------------------------------------------------
// RUST-P7-002 S13 — audio response classification & writer selection.
//
// Mirrors the dispatch Go's `OutboundTransformer.TransformResponse`
// (outbound.go:261-266) performs on `api_format` for the three audio
// endpoints, plus the `Accept` header construction in
// `buildSpeechRequest` (audio_outbound.go:62-68) and
// `buildAudioMultipartRequest` (audio_outbound.go:223-227):
//
//   * `OpenAiAudioSpeech`       → binary stream (audio/* Content-Type)
//   * `OpenAiAudioTranscriptions` / `OpenAiAudioTranslations`
//                               → JSON object (no SSE writer)
//
// The Rust transformer layer does not yet host the full
// `TransformResponse`/`TransformStream` trait wiring, so these helpers
// capture the *pure* classification + content-type selection logic that
// the future wiring will compose. They are deliberately side-effect-free
// (no I/O, no SSE writer) so they can be unit-tested directly against the
// Go golden cases in `audio_outbound_test.go`.
// ---------------------------------------------------------------------------

/// Output channel an audio response must use.
///
/// Mirrors the two distinct branches Go's `TransformResponse` takes for
/// audio (outbound.go:261-266):
///
/// * [`AudioResponseMode::Binary`] — speech TTS response is a raw audio byte
///   stream; the response body is forwarded untouched with an `audio/*`
///   Content-Type (Go `transformSpeechResponse`, audio_outbound.go:377-391).
/// * [`AudioResponseMode::Json`] — transcription/translation response is a
///   JSON object (or plain text for `text`/`srt`/`vtt` formats); it is never
///   written through the SSE writer (Go `transformTranscriptionResponse`,
///   audio_outbound.go:395-438, returns a single `*llm.Response`, not a
///   stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioResponseMode {
    Binary,
    Json,
}

/// Classify the audio response mode from the request's `api_format`,
/// mirroring Go's `TransformResponse` switch (outbound.go:261-266):
///
/// * `OpenAiAudioSpeech` → [`AudioResponseMode::Binary`]
/// * `OpenAiAudioTranscriptions` / `OpenAiAudioTranslations`
///   → [`AudioResponseMode::Json`]
///
/// Non-audio formats return `None` (Go's switch falls through to the
/// chat/embedding branches); callers outside the audio family should not
/// reach this classifier.
pub fn classify_audio_response_format(api_format: ApiFormat) -> Option<AudioResponseMode> {
    match api_format {
        ApiFormat::OpenAiAudioSpeech => Some(AudioResponseMode::Binary),
        ApiFormat::OpenAiAudioTranscriptions | ApiFormat::OpenAiAudioTranslations => {
            Some(AudioResponseMode::Json)
        }
        _ => None,
    }
}

/// Pick the response Content-Type for a binary audio (speech) response,
/// mirroring Go `transformSpeechResponse` (audio_outbound.go:377-391):
///
/// * If the upstream `Content-Type` header is present and non-empty, it is
///   used verbatim (so `audio/wav`, `audio/ogg`, … round-trip untouched).
/// * Otherwise the default is `audio/mpeg` (Go: `contentType = "audio/mpeg"`).
///
/// Only valid for [`AudioResponseMode::Binary`]; callers are responsible
/// for routing through [`classify_audio_response_format`] first.
pub fn speech_response_content_type(upstream_content_type: Option<&str>) -> String {
    match upstream_content_type
        .map(str::trim)
        .filter(|ct| !ct.is_empty())
    {
        Some(ct) => ct.to_string(),
        None => "audio/mpeg".to_string(),
    }
}

/// Pick the `Accept` header for an outbound audio request, mirroring Go's
/// per-endpoint Accept construction:
///
/// * Speech (`buildSpeechRequest`, audio_outbound.go:62-68):
///   * `stream_format == "sse"` → `text/event-stream`
///   * otherwise (binary or unset) → `*/*`
/// * Transcription/Translation (`buildAudioMultipartRequest`,
///   audio_outbound.go:223-227):
///   * `stream == true` → `text/event-stream`
///   * otherwise → `application/json`
///
/// `stream` for STT endpoints is forwarded as a multipart field
/// (audio_outbound.go:117-119); for speech the `sse`/`audio` split is
/// carried by `stream_format`.
pub fn audio_request_accept_header(
    api_format: ApiFormat,
    speech_stream_format: Option<&str>,
    stt_stream: bool,
) -> String {
    match api_format {
        ApiFormat::OpenAiAudioSpeech => {
            if speech_stream_format.map(str::trim).unwrap_or("") == "sse" {
                "text/event-stream".to_string()
            } else {
                "*/*".to_string()
            }
        }
        ApiFormat::OpenAiAudioTranscriptions | ApiFormat::OpenAiAudioTranslations => {
            if stt_stream {
                "text/event-stream".to_string()
            } else {
                "application/json".to_string()
            }
        }
        // Non-audio formats should not route here; return the OpenAI default.
        _ => "application/json".to_string(),
    }
}

/// Confirm the JSON mode for STT responses never selects an SSE writer.
///
/// Go's `transformTranscriptionResponse` (audio_outbound.go:395-438) always
/// returns a single `*llm.Response`, even when the inbound request had
/// `stream=true`. The streaming variant is handled by a separate
/// `TransformStream` path; the non-streaming response handler must never
/// touch the SSE writer. This helper codifies that invariant so future
/// response-wiring code can assert it cheaply.
pub const fn audio_json_mode_is_non_streaming(mode: AudioResponseMode) -> bool {
    match mode {
        AudioResponseMode::Json => true,
        AudioResponseMode::Binary => false,
    }
}

// ---------------------------------------------------------------------------
// Tests — mirror Go `outbound_test.go` (auth header / URL cases) and
// `usage_test.go` (usage extraction). All Go `*_test.go` cases are covered
// unless explicitly noted.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::TokenDetails;
    use serde_json::json;
    use std::collections::BTreeMap;

    // ---- S06 build_auth_header --------------------------------------------

    #[test]
    fn s06_build_auth_header_sets_bearer_content_type_and_accept() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "valid request with default URL"
        // which asserts `req.Auth.Type == "bearer"`, `req.Auth.APIKey ==
        // "test-api-key"`, plus the Content-Type header Go sets at
        // outbound.go:206-207.
        let (headers, auth) = build_auth_header("test-api-key", "openai")?;

        assert_eq!(
            headers.get("Content-Type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(headers.get("Accept"), Some(&"application/json".to_string()));
        assert_eq!(
            headers.get("Authorization"),
            Some(&"Bearer test-api-key".to_string())
        );
        assert_eq!(auth.scheme, "bearer");
        assert_eq!(auth.token.as_deref(), Some("test-api-key"));
        Ok(())
    }

    #[test]
    fn s06_build_auth_header_rejects_empty_api_key() {
        // Mirrors Go `validateConfig` (outbound.go:122-124): "API key provider
        // is required". Here we surface the same guard at the header builder.
        let err = build_auth_header("", "openai").err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
        assert!(
            err.map(|err| err.public_message().to_lowercase())
                .map_or(false, |message| message.contains("api key"))
        );
    }

    #[test]
    fn s06_build_auth_header_bearer_scheme_for_all_openai_compatible_channels()
    -> TransformerResult<()> {
        // Go hardcodes `Type: "bearer"` for every OpenAI-compatible channel
        // (outbound.go:210). Verify the scheme is stable across channels that
        // the Rust side may key differently (openai, deepseek, moonshot,
        // google's OpenAI-compat endpoint).
        for channel in ["openai", "deepseek", "moonshot", "google"] {
            let (_, auth) = build_auth_header("k", channel)?;
            assert_eq!(auth.scheme, "bearer", "channel {channel}");
        }
        Ok(())
    }

    // ---- S07 resolve_outbound_url -----------------------------------------

    #[test]
    fn s07_resolve_default_chat_completions_url() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "valid request with default URL":
        // base `https://api.openai.com/v1` (v1 already present) →
        // `https://api.openai.com/v1/chat/completions`.
        let url = resolve_outbound_url("https://api.openai.com/v1".to_string(), None, false)?;
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        Ok(())
    }

    #[test]
    fn s07_resolve_appends_v1_when_missing() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "raw URL false with standard URL":
        // base `https://api.openai.com` (no version) →
        // `https://api.openai.com/v1/chat/completions`.
        let url = resolve_outbound_url("https://api.openai.com".to_string(), None, false)?;
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        Ok(())
    }

    #[test]
    fn s07_resolve_strips_trailing_slash() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "URL with trailing slash":
        // `https://api.openai.com/v1/` → `.../v1/chat/completions` (no
        // double slash).
        let url = resolve_outbound_url("https://api.openai.com/v1/".to_string(), None, false)?;
        assert_eq!(url, "https://api.openai.com/v1/chat/completions");
        Ok(())
    }

    #[test]
    fn s07_resolve_preserves_nested_version_path() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "URL with /v1/ ":
        // `https://api.deepinfra.com/v1/openai` (v1 mid-path) →
        // `https://api.deepinfra.com/v1/openai/chat/completions`. Go
        // `NormalizeBaseURL` detects `/v1/` in the path and skips appending
        // another `v1`.
        let url = resolve_outbound_url(
            "https://api.deepinfra.com/v1/openai".to_string(),
            None,
            false,
        )?;
        assert_eq!(url, "https://api.deepinfra.com/v1/openai/chat/completions");
        Ok(())
    }

    #[test]
    fn s07_resolve_custom_endpoint_skips_default_version() -> TransformerResult<()> {
        // Mirrors Go outbound.go:102-105: when `EndpointPath` is set, base URL
        // is normalized with version `""` (no `v1` appended) and the custom
        // path is appended verbatim.
        let url = resolve_outbound_url(
            "https://custom-endpoint.com/api/llm".to_string(),
            Some("/v2/chat"),
            false,
        )?;
        assert_eq!(url, "https://custom-endpoint.com/api/llm/v2/chat");
        Ok(())
    }

    #[test]
    fn s07_resolve_raw_url_returns_base_verbatim() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "raw URL enabled with Config":
        // `RawURL: true` → base returned as-is (no `/chat/completions`).
        let url = resolve_outbound_url("https://custom.api.com/v1".to_string(), None, true)?;
        assert_eq!(url, "https://custom.api.com/v1");
        Ok(())
    }

    #[test]
    fn s07_resolve_hash_hash_suffix_forces_raw_url() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "raw URL with custom endpoint
        // without version": base `.../api/llm##` → `##` stripped, raw URL,
        // no path appended.
        let url = resolve_outbound_url(
            "https://custom-endpoint.com/api/llm##".to_string(),
            None,
            false,
        )?;
        assert_eq!(url, "https://custom-endpoint.com/api/llm");
        Ok(())
    }

    #[test]
    fn s07_resolve_single_hash_strips_marker_and_skips_version() -> TransformerResult<()> {
        // Mirrors Go outbound_test.go case "raw base URL with custom endpoint
        // without version": base `.../api/llm#` → `#` stripped, then default
        // `/chat/completions` path appended (NormalizeBaseURL with `#` skips
        // version).
        let url = resolve_outbound_url(
            "https://custom-endpoint.com/api/llm#".to_string(),
            None,
            false,
        )?;
        assert_eq!(url, "https://custom-endpoint.com/api/llm/chat/completions");
        Ok(())
    }

    #[test]
    fn s07_normalize_base_url_mirrors_go_url_test_cases() {
        // Direct mirrors of representative Go `url_test.go::TestNormalizeBaseURL`
        // cases, exercising each branch of Go `NormalizeBaseURL` (url.go).
        let cases: &[(&str, &str, &str)] = &[
            // (input, version, expected)
            ("", "v1", ""),
            (
                "https://api.example.com/",
                "v1",
                "https://api.example.com/v1",
            ),
            (
                "https://api.example.com",
                "v1",
                "https://api.example.com/v1",
            ),
            (
                "https://api.example.com/v1",
                "v1",
                "https://api.example.com/v1",
            ),
            (
                "https://api.example.com/v1/openai",
                "v1",
                "https://api.example.com/v1/openai",
            ),
            (
                "https://api.example.com/v1/openai/",
                "v1",
                "https://api.example.com/v1/openai",
            ),
            // `#` marker — strip + skip version, regardless of version arg.
            (
                "https://api.example.com/v1#",
                "",
                "https://api.example.com/v1",
            ),
            ("https://api.example.com#", "v1", "https://api.example.com"),
            ("https://api.example.com/#", "v1", "https://api.example.com"),
            // empty version → trim only.
            ("https://api.example.com", "", "https://api.example.com"),
            ("https://api.example.com/", "", "https://api.example.com"),
            // multiple trailing slashes.
            (
                "https://api.example.com///",
                "v1",
                "https://api.example.com/v1",
            ),
            // port handling.
            (
                "https://api.example.com:8080",
                "v1",
                "https://api.example.com:8080/v1",
            ),
            (
                "https://api.example.com:8080/",
                "v1",
                "https://api.example.com:8080/v1",
            ),
            (
                "https://api.example.com:8080/v1",
                "v1",
                "https://api.example.com:8080/v1",
            ),
            // version mismatch — Go appends the requested version verbatim.
            (
                "https://api.example.com/v1",
                "v2",
                "https://api.example.com/v1/v2",
            ),
            // Azure-style composite version `openai/v1`.
            (
                "https://my-resource.openai.azure.com",
                "openai/v1",
                "https://my-resource.openai.azure.com/openai/v1",
            ),
            (
                "https://my-resource.openai.azure.com/openai/v1",
                "openai/v1",
                "https://my-resource.openai.azure.com/openai/v1",
            ),
        ];
        for (input, version, expected) in cases {
            let got = normalize_base_url((*input).to_string(), version);
            assert_eq!(
                got, *expected,
                "normalize_base_url({input:?}, {version:?}) mismatch"
            );
        }
    }

    // ---- S10 extract_usage -------------------------------------------------

    /// Helper: build a `serde_json::Value` object wrapping a `usage` payload,
    /// matching how the field appears inside a real OpenAI chat completion
    /// response body.
    fn response_with_usage(usage: Value) -> Value {
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "model": "gpt-4",
            "choices": [],
            "usage": usage,
        })
    }

    #[test]
    fn s10_extract_usage_returns_none_when_no_usage_field() {
        // Mirrors Go behavior: a response with no `usage` JSON object yields a
        // nil `*llm.Usage` — the Go code simply never constructs one.
        let response = json!({"id": "x", "object": "chat.completion"});
        assert_eq!(extract_usage(&response), None);
    }

    #[test]
    fn s10_extract_usage_basic_without_details() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "basic usage without cached tokens".
        let response = response_with_usage(json!({
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
        // No structured details → details remain at zero defaults.
        assert!(usage.prompt_details.is_zero());
        assert!(usage.completion_details.is_zero());
        Ok(())
    }

    #[test]
    fn s10_extract_usage_accepts_responses_api_field_names() -> TransformerResult<()> {
        let usage = extract_usage(&json!({
            "usage": {
                "input_tokens": 120,
                "input_tokens_details": {"cached_tokens": 20},
                "output_tokens": 40,
                "output_tokens_details": {"reasoning_tokens": 12},
                "total_tokens": 160
            }
        }))
        .ok_or_else(|| ConduitError::internal("responses usage was not extracted"))?;
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.total_tokens, 160);
        assert_eq!(usage.prompt_details.cached_tokens, 20);
        assert_eq!(usage.completion_details.reasoning_tokens, 12);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_folds_moonshot_cached_tokens_when_details_absent() -> TransformerResult<()>
    {
        // Mirrors Go usage_test.go case "usage with cached tokens and no
        // existing details": top-level `cached_tokens` should be folded into
        // `prompt_details.cached_tokens`.
        let response = response_with_usage(json!({
            "prompt_tokens": 15,
            "completion_tokens": 25,
            "total_tokens": 40,
            "cached_tokens": 5,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_details.cached_tokens, 5);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_does_not_overwrite_existing_cached_tokens() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with cached tokens and existing
        // details - cached tokens not overwritten": when the structured
        // `prompt_tokens_details.cached_tokens` is non-zero, the top-level
        // Moonshot `cached_tokens` must NOT override it.
        let response = response_with_usage(json!({
            "prompt_tokens": 20,
            "completion_tokens": 30,
            "total_tokens": 50,
            "prompt_tokens_details": {"cached_tokens": 2},
            "cached_tokens": 8,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_details.cached_tokens, 2);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_zero_moonshot_cached_tokens_leaves_details_empty() -> TransformerResult<()>
    {
        // Mirrors Go usage_test.go case "usage with zero cached tokens".
        let response = response_with_usage(json!({
            "prompt_tokens": 12,
            "completion_tokens": 18,
            "total_tokens": 30,
            "cached_tokens": 0,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_tokens, 12);
        assert!(usage.prompt_details.is_zero());
        Ok(())
    }

    #[test]
    fn s10_extract_usage_prompt_tokens_details() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with prompt tokens details".
        let response = response_with_usage(json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {"audio_tokens": 10, "cached_tokens": 20},
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_details.audio_tokens, 10);
        assert_eq!(usage.prompt_details.cached_tokens, 20);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_completion_tokens_details() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with completion tokens
        // details".
        let response = response_with_usage(json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "completion_tokens_details": {
                "audio_tokens": 5,
                "reasoning_tokens": 10,
                "accepted_prediction_tokens": 3,
                "rejected_prediction_tokens": 2
            },
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.completion_details.audio_tokens, 5);
        assert_eq!(usage.completion_details.reasoning_tokens, 10);
        assert_eq!(usage.completion_details.accepted_prediction_tokens, 3);
        assert_eq!(usage.completion_details.rejected_prediction_tokens, 2);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_all_details_combined() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with all details".
        let response = response_with_usage(json!({
            "prompt_tokens": 200,
            "completion_tokens": 100,
            "total_tokens": 300,
            "prompt_tokens_details": {"audio_tokens": 20, "cached_tokens": 30},
            "completion_tokens_details": {
                "audio_tokens": 10,
                "reasoning_tokens": 20,
                "accepted_prediction_tokens": 5,
                "rejected_prediction_tokens": 5
            },
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_tokens, 200);
        assert_eq!(usage.completion_tokens, 100);
        assert_eq!(usage.total_tokens, 300);
        assert_eq!(usage.prompt_details.audio_tokens, 20);
        assert_eq!(usage.prompt_details.cached_tokens, 30);
        assert_eq!(usage.completion_details.audio_tokens, 10);
        assert_eq!(usage.completion_details.reasoning_tokens, 20);
        assert_eq!(usage.completion_details.accepted_prediction_tokens, 5);
        assert_eq!(usage.completion_details.rejected_prediction_tokens, 5);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_moonshot_folds_when_structured_cached_is_zero() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with cached tokens and zero
        // cached tokens in details": structured details present but
        // cached_tokens=0, top-level cached_tokens=15 → fold.
        let response = response_with_usage(json!({
            "prompt_tokens": 50,
            "completion_tokens": 30,
            "total_tokens": 80,
            "prompt_tokens_details": {"cached_tokens": 0},
            "cached_tokens": 15,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        // The Go check (usage.go:62) is `prompt_details.cached_tokens == 0
        // && cached_tokens > 0` → fold. We also expect the audio_tokens branch
        // NOT to suppress this (structured object is non-zero overall, but
        // cached_tokens within it is zero — Go explicitly checks the cached
        // field, not the struct).
        assert_eq!(usage.prompt_details.cached_tokens, 15);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_write_cached_tokens_preserved() -> TransformerResult<()> {
        // Mirrors Go usage_test.go case "usage with write cached tokens".
        let response = response_with_usage(json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {
                "audio_tokens": 10,
                "cached_tokens": 20,
                "write_cached_tokens": 5
            },
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.prompt_details.audio_tokens, 10);
        assert_eq!(usage.prompt_details.cached_tokens, 20);
        assert_eq!(usage.prompt_details.write_cached_tokens, 5);
        Ok(())
    }

    #[test]
    fn s10_extract_usage_preserves_unknown_provider_fields() -> TransformerResult<()> {
        // Extra fields (e.g. Anthropic-via-OpenAI-proxy `cache_creation_input_tokens`
        // or a provider cost line) round-trip via the unified Usage `extra`
        // flatten. There is no direct Go golden case (Go drops unknown fields
        // because its `Usage` struct has no flatten), but the Rust unified
        // model guarantees losslessness — exercise it.
        let response = response_with_usage(json!({
            "prompt_tokens": 7,
            "completion_tokens": 11,
            "total_tokens": 18,
            "provider_cost_usd": 0.002,
        }));
        let usage = extract_usage(&response)
            .ok_or_else(|| ConduitError::internal("expected Some(usage)"))?;

        assert_eq!(usage.extra.get("provider_cost_usd"), Some(&json!(0.002)));
        Ok(())
    }

    // ---- S10 usage_from_llm (Go TestUsageFromLLM parity, L218-384) ---------
    //
    // Go `UsageFromLLM(u *llm.Usage) *Usage` (usage.go:74-103) converts a
    // unified `llm.Usage` to the OpenAI-compatible wire `Usage`. In Rust's
    // unified architecture there is no separate `openai.Usage` type — the
    // unified `Usage` IS the OpenAI wire format (with serde renames
    // `prompt_tokens_details` / `completion_tokens_details`). These tests
    // verify the serialization direction: building a unified `Usage` with
    // the fields Go's `UsageFromLLM` would set, then asserting the wire JSON
    // carries every field under the correct key. This directly mirrors each
    // Go `TestUsageFromLLM` golden case (usage_test.go:218-384).

    // Mirrors Go usage_test.go:224-228 "nil usage returns nil":
    // `UsageFromLLM(nil)` returns `nil`. In Rust, a `None` usage serializes
    // to JSON `null`. (The inbound counterpart — a response without a `usage`
    // object yields `None` — is covered by
    // `s10_extract_usage_returns_none_when_no_usage_field` above.)
    #[test]
    fn s10_usage_from_llm_none_serializes_to_null() -> Result<(), serde_json::Error> {
        let none: Option<Usage> = None;
        let wire = serde_json::to_value(&none)?;
        assert!(wire.is_null());
        Ok(())
    }

    // Mirrors Go usage_test.go:229-241 "basic usage without details": a
    // unified `Usage` with just the three token counts serializes them at the
    // top level. Go's `UsageFromLLM` zero-fills the details structs; in Rust
    // the default `TokenDetails` always serializes (no `skip_serializing_if`)
    // — an architectural choice, not a parity bug.
    #[test]
    fn s10_usage_from_llm_basic_without_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens"], 10);
        assert_eq!(wire["completion_tokens"], 20);
        assert_eq!(wire["total_tokens"], 30);
        Ok(())
    }

    // Mirrors Go usage_test.go:242-262 "usage with prompt tokens details":
    // `PromptTokensDetails{AudioTokens:10, CachedTokens:20}` serializes under
    // the `prompt_tokens_details` key (Go json tag, Rust serde rename).
    #[test]
    fn s10_usage_from_llm_prompt_tokens_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_details: TokenDetails {
                audio_tokens: 10,
                cached_tokens: 20,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens"], 100);
        assert_eq!(wire["completion_tokens"], 50);
        assert_eq!(wire["total_tokens"], 150);
        assert_eq!(wire["prompt_tokens_details"]["audio_tokens"], 10);
        assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 20);
        Ok(())
    }

    // Mirrors Go usage_test.go:263-287 "usage with completion tokens
    // details": `CompletionTokensDetails{AudioTokens:5, ReasoningTokens:10,
    // AcceptedPredictionTokens:3, RejectedPredictionTokens:2}` serializes
    // under the `completion_tokens_details` key.
    #[test]
    fn s10_usage_from_llm_completion_tokens_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            completion_details: TokenDetails {
                audio_tokens: 5,
                reasoning_tokens: 10,
                accepted_prediction_tokens: 3,
                rejected_prediction_tokens: 2,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["completion_tokens_details"]["audio_tokens"], 5);
        assert_eq!(wire["completion_tokens_details"]["reasoning_tokens"], 10);
        assert_eq!(
            wire["completion_tokens_details"]["accepted_prediction_tokens"],
            3
        );
        assert_eq!(
            wire["completion_tokens_details"]["rejected_prediction_tokens"],
            2
        );
        Ok(())
    }

    // Mirrors Go usage_test.go:288-320 "usage with all details": both
    // prompt_details and completion_details populated.
    #[test]
    fn s10_usage_from_llm_all_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            prompt_details: TokenDetails {
                audio_tokens: 20,
                cached_tokens: 30,
                ..TokenDetails::default()
            },
            completion_details: TokenDetails {
                audio_tokens: 10,
                reasoning_tokens: 20,
                accepted_prediction_tokens: 5,
                rejected_prediction_tokens: 5,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens"], 200);
        assert_eq!(wire["completion_tokens"], 100);
        assert_eq!(wire["total_tokens"], 300);
        assert_eq!(wire["prompt_tokens_details"]["audio_tokens"], 20);
        assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 30);
        assert_eq!(wire["completion_tokens_details"]["audio_tokens"], 10);
        assert_eq!(wire["completion_tokens_details"]["reasoning_tokens"], 20);
        assert_eq!(
            wire["completion_tokens_details"]["accepted_prediction_tokens"],
            5
        );
        assert_eq!(
            wire["completion_tokens_details"]["rejected_prediction_tokens"],
            5
        );
        Ok(())
    }

    // Mirrors Go usage_test.go:321-339 "usage with zero cached tokens in
    // details": `PromptTokensDetails{CachedTokens:0}` is preserved. The zero
    // value serializes as `"cached_tokens": 0` (Go has no `omitempty` on
    // `CachedTokens` in either the unified or OpenAI details struct).
    #[test]
    fn s10_usage_from_llm_zero_cached_tokens_in_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 50,
            completion_tokens: 30,
            total_tokens: 80,
            prompt_details: TokenDetails {
                cached_tokens: 0,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens"], 50);
        assert_eq!(wire["completion_tokens"], 30);
        assert_eq!(wire["total_tokens"], 80);
        assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 0);
        Ok(())
    }

    // Mirrors Go usage_test.go:340-352 "usage with nil prompt tokens
    // details": in Go, `UsageFromLLM` with nil `PromptTokensDetails` produces
    // a `Usage` with zero-valued (but present) details. In Rust the default
    // `TokenDetails` carries the same zero values; the three token counts
    // are the meaningful fields.
    #[test]
    fn s10_usage_from_llm_nil_prompt_tokens_details() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 75,
            completion_tokens: 25,
            total_tokens: 100,
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens"], 75);
        assert_eq!(wire["completion_tokens"], 25);
        assert_eq!(wire["total_tokens"], 100);
        Ok(())
    }

    // Mirrors Go usage_test.go:353-375 "usage with write cached tokens":
    // `PromptTokensDetails{AudioTokens:10, CachedTokens:20,
    // WriteCachedTokens:5}` serializes `write_cached_tokens` under
    // `prompt_tokens_details`. Go's `llm.PromptTokensDetails` has `omitempty`
    // on `WriteCachedTokens`; the Rust unified `TokenDetails` does not
    // (architectural choice for losslessness), but the value is preserved
    // correctly.
    #[test]
    fn s10_usage_from_llm_write_cached_tokens() -> Result<(), serde_json::Error> {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            prompt_details: TokenDetails {
                audio_tokens: 10,
                cached_tokens: 20,
                write_cached_tokens: 5,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let wire = serde_json::to_value(&usage)?;
        assert_eq!(wire["prompt_tokens_details"]["audio_tokens"], 10);
        assert_eq!(wire["prompt_tokens_details"]["cached_tokens"], 20);
        assert_eq!(wire["prompt_tokens_details"]["write_cached_tokens"], 5);
        Ok(())
    }

    // ---- apply_outbound_transport (composition) ----------------------------

    #[test]
    fn apply_outbound_transport_sets_method_url_headers_auth() -> TransformerResult<()> {
        // End-to-end composition of S06 + S07 onto an HttpRequest, mirroring
        // the tail of Go `OutboundTransformer.TransformRequest`
        // (outbound.go:220-228).
        let config = Config {
            platform_type: PlatformType::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            raw_url: false,
            endpoint_path: None,
            api_key: "test-api-key".to_string(),
        };
        let request = apply_outbound_transport(HttpRequest::default(), &config, "openai")?;

        assert_eq!(request.method, "POST");
        assert_eq!(
            request.url.as_deref(),
            Some("https://api.openai.com/v1/chat/completions")
        );
        assert_eq!(
            request.headers.get("Authorization"),
            Some(&"Bearer test-api-key".to_string())
        );
        assert_eq!(
            request.auth.as_ref().map(|auth| auth.scheme.as_str()),
            Some("bearer")
        );
        assert_eq!(
            request.auth.and_then(|auth| auth.token),
            Some("test-api-key".to_string())
        );
        Ok(())
    }

    #[test]
    fn apply_outbound_transport_validates_config() {
        let config = Config {
            platform_type: PlatformType::OpenAi,
            base_url: String::new(),
            raw_url: false,
            endpoint_path: None,
            api_key: "k".to_string(),
        };
        let err = apply_outbound_transport(HttpRequest::default(), &config, "openai").err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
    }

    // ---- PlatformType / Config validation ---------------------------------

    #[test]
    fn platform_type_parse_accepts_openai_and_google() -> TransformerResult<()> {
        assert_eq!(PlatformType::parse("openai")?, PlatformType::OpenAi);
        assert_eq!(PlatformType::parse("google")?, PlatformType::Google);
        // Empty string defaults to OpenAI (mirrors Go zero-value
        // `PlatformType("")` falling through the switch's default case via the
        // OpenAI default in NewOutboundTransformer).
        assert_eq!(PlatformType::parse("")?, PlatformType::OpenAi);
        Ok(())
    }

    #[test]
    fn platform_type_parse_rejects_unknown() {
        // Mirrors Go validateConfig (outbound.go:129-134): "unsupported
        // platform type".
        let err = PlatformType::parse("claude").err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
    }

    #[test]
    fn config_validate_rejects_missing_base_url_and_api_key() {
        let no_base = Config {
            platform_type: PlatformType::OpenAi,
            base_url: String::new(),
            raw_url: false,
            endpoint_path: None,
            api_key: "k".to_string(),
        };
        assert!(no_base.validate().is_err());

        let no_key = Config {
            platform_type: PlatformType::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            raw_url: false,
            endpoint_path: None,
            api_key: String::new(),
        };
        assert!(no_key.validate().is_err());
    }

    // ---- config_test.go::TestValidateConfig parity ------------------------
    //
    // The Go `validateConfig` (outbound.go:115-135) enforces four rules:
    //   1. non-nil config            → "config cannot be nil"
    //   2. non-nil APIKeyProvider    → "API key provider is required"
    //   3. non-empty BaseURL         → "base URL is required"
    //   4. supported PlatformType    → "unsupported platform type: %v"
    //
    // In Rust rule 1 is inexpressible (no nil); rule 2 maps to checking
    // `api_key.is_empty()`; rule 4 is enforced at `PlatformType::parse` time
    // (the enum has only `OpenAi`/`Google` variants, so `validate()` itself
    // doesn't re-check). Below we mirror each Go subtest's spirit through the
    // Rust `Config::validate()` / `PlatformType::parse` equivalents.

    // Mirrors Go config_test.go:26-33 "valid OpenAI config": a Config with
    // PlatformType=OpenAI, APIKeyProvider set, and a BaseURL passes
    // validation. Rust uses `api_key: String` instead of an `APIKeyProvider`
    // trait object; a non-empty key is the equivalent of a non-nil provider.
    #[test]
    fn config_validate_accepts_valid_openai_config() -> TransformerResult<()> {
        let config = Config {
            platform_type: PlatformType::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            raw_url: false,
            endpoint_path: None,
            api_key: "test-api-key".to_string(),
        };
        config.validate()?;
        Ok(())
    }

    // Mirrors Go config_test.go:53-61 "unsupported platform type": a Config
    // with an unsupported platform string is rejected. In Rust the platform
    // is a constrained enum, so the rejection happens at `PlatformType::parse`
    // (the equivalent of Go's runtime switch in `validateConfig`).
    #[test]
    fn config_validate_rejects_unsupported_platform_type() {
        // PlatformType::parse is the gate that mirrors Go's
        // `switch config.PlatformType { case PlatformOpenAI, PlatformGoogle: … }`.
        let err = PlatformType::parse("invalid-platform").err();
        assert!(
            err.is_some(),
            "expected error for unsupported platform type"
        );
        let err_msg = err.as_ref().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            err_msg.contains("unsupported platform type"),
            "error should mention 'unsupported platform type', got: {err_msg}"
        );
    }

    // Mirrors Go config_test.go:43-50 "OpenAI config missing base URL": the
    // error message must contain "base URL is required" (byte-exact match with
    // Go outbound.go:127).
    #[test]
    fn config_validate_error_message_is_base_url_is_required() {
        let config = Config {
            platform_type: PlatformType::OpenAi,
            base_url: String::new(),
            raw_url: false,
            endpoint_path: None,
            api_key: "test-api-key".to_string(),
        };
        match config.validate() {
            Err(err) => assert!(
                err.to_string().contains("base URL is required"),
                "error should contain 'base URL is required', got: {err}"
            ),
            Ok(_) => panic!("expected base-URL-required error"),
        }
    }

    // Mirrors Go config_test.go:35-41 "OpenAI config missing API key
    // provider": Go's message is "API key provider is required". The Rust
    // unified model replaces the `APIKeyProvider` trait object with a plain
    // `api_key: String`, so the message is "API key is required" — documenting
    // this intentional divergence from the Go error string.
    #[test]
    fn config_validate_error_message_for_missing_api_key() {
        let config = Config {
            platform_type: PlatformType::OpenAi,
            base_url: "https://api.openai.com/v1".to_string(),
            raw_url: false,
            endpoint_path: None,
            api_key: String::new(),
        };
        match config.validate() {
            Err(err) => assert!(
                err.to_string().contains("API key is required"),
                "error should contain 'API key is required', got: {err}"
            ),
            Ok(_) => panic!("expected API-key-required error"),
        }
    }

    // ---- Pending Go-specific config_test.go cases (catalogue) -------------
    //
    // The following Go config_test.go tests exercise Go-specific APIs that
    // have no direct Rust equivalent and are pending a future port wave:
    //
    // pending: TestNewOutboundTransformerWithConfig_Validation
    //   (config_test.go:81-136) — Go constructor `NewOutboundTransformerWithConfig`
    //   validates config AND normalizes the BaseURL (appending `v1`, handling
    //   `##` suffix). The Rust side splits these: `Config::validate()` checks
    //   the fields, and `resolve_outbound_url` handles URL normalization.
    //   No single constructor function combines both yet (RUST-P7-002 S08).
    //
    // pending: TestSetConfig_Validation (config_test.go:138-179)
    //   — Go's `SetConfig` panics on invalid config (Go's `require.Panics`
    //   idiom). Rust uses an immutable `Config` + explicit `validate()` call,
    //   so there is no panic-on-invalid-setter to test.
    //
    // pending: TestSetAPIKey_Validation (config_test.go:181-205)
    //   — Go's `SetAPIKey` mutates the `APIKeyProvider` in place. Rust uses an
    //   immutable `api_key: String`, so there is no mutable setter to test.

    // Sanity: confirm HeaderMap is the expected BTreeMap alias and no helper
    // imports are missing.
    #[test]
    fn header_map_alias_compiles() {
        let _map: HeaderMap = BTreeMap::new();
    }

    // ---- S13 audio response classification & writer selection ------------

    // Mirrors Go `OutboundTransformer.TransformResponse` dispatch
    // (outbound.go:261-266): the three audio `api_format` values map to the
    // two distinct response writers. Speech → binary stream; transcription
    // and translation → JSON object. Non-audio formats are out of scope.
    #[test]
    fn s13_classify_audio_response_format_dispatches_like_go() {
        use AudioResponseMode::{Binary, Json};

        // Speech → Binary (audio_outbound.go:261-262).
        assert_eq!(
            classify_audio_response_format(ApiFormat::OpenAiAudioSpeech),
            Some(Binary)
        );
        // Transcription → Json (audio_outbound.go:263-264).
        assert_eq!(
            classify_audio_response_format(ApiFormat::OpenAiAudioTranscriptions),
            Some(Json)
        );
        // Translation → Json (audio_outbound.go:265-266).
        assert_eq!(
            classify_audio_response_format(ApiFormat::OpenAiAudioTranslations),
            Some(Json)
        );
        // Non-audio formats are not classified here (Go's switch falls through
        // to chat/embedding handling).
        assert_eq!(
            classify_audio_response_format(ApiFormat::OpenAiChatCompletions),
            None
        );
    }

    // Mirrors Go `TestOutbound_TransformSpeechResponse`
    // (audio_outbound_test.go:155-171): the upstream `Content-Type` header
    // (e.g. `audio/wav`) is preserved verbatim on the binary speech response,
    // and defaults to `audio/mpeg` when the provider omits it.
    #[test]
    fn s13_speech_response_content_type_preserves_upstream_then_defaults() {
        // Upstream Content-Type wins.
        assert_eq!(speech_response_content_type(Some("audio/wav")), "audio/wav");
        assert_eq!(speech_response_content_type(Some("audio/ogg")), "audio/ogg");
        // Missing header → Go default `audio/mpeg` (audio_outbound.go:379-380).
        assert_eq!(speech_response_content_type(None), "audio/mpeg");
        // Empty / whitespace-only header → Go trims then falls back.
        assert_eq!(speech_response_content_type(Some("")), "audio/mpeg");
        assert_eq!(speech_response_content_type(Some("   ")), "audio/mpeg");
    }

    // Mirrors Go `TestAudioStreaming_Speech` (audio_outbound_test.go:327) and
    // `TestAudioStreaming_SpeechBinaryChunks` (audio_outbound_test.go:398):
    // the speech Accept header is `text/event-stream` for `stream_format=sse`
    // and `*/*` for binary / `stream_format=audio` / unset.
    #[test]
    fn s13_speech_accept_header_sse_vs_binary() {
        // stream_format=sse → SSE (audio_outbound.go:62-65).
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioSpeech, Some("sse"), false),
            "text/event-stream"
        );
        // stream_format=audio → binary, Accept */* (audio_outbound.go:66-68).
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioSpeech, Some("audio"), false),
            "*/*"
        );
        // Unset stream_format → binary (non-streaming TTS).
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioSpeech, None, false),
            "*/*"
        );
    }

    // Mirrors Go `TestAudioStreaming_Transcription`
    // (audio_outbound_test.go:463): the STT Accept header is
    // `text/event-stream` when stream=true and `application/json` otherwise
    // (audio_outbound.go:223-227). Verified for both transcription and
    // translation endpoints.
    #[test]
    fn s13_stt_accept_header_json_vs_sse() {
        // Transcription: streaming → SSE.
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioTranscriptions, None, true),
            "text/event-stream"
        );
        // Transcription: non-streaming → JSON.
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioTranscriptions, None, false),
            "application/json"
        );
        // Translation: streaming → SSE.
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioTranslations, None, true),
            "text/event-stream"
        );
        // Translation: non-streaming → JSON.
        assert_eq!(
            audio_request_accept_header(ApiFormat::OpenAiAudioTranslations, None, false),
            "application/json"
        );
    }

    // Mirrors Go `TestOutbound_TransformTranscriptionResponse`
    // (audio_outbound_test.go:173-267): the non-streaming STT response is a
    // single JSON object, never routed through the SSE writer. JSON formats
    // (json / verbose_json) are parsed; text/srt/vtt pass through raw. The
    // classifier + invariant guard codify that JSON mode must remain
    // non-streaming so future response wiring cannot accidentally mix in the
    // SSE writer.
    #[test]
    fn s13_audio_json_mode_is_non_streaming_invariant() {
        // JSON mode (transcription/translation) must never select an SSE
        // writer — Go returns a single `*llm.Response`, not a stream.
        assert!(audio_json_mode_is_non_streaming(AudioResponseMode::Json));
        // Binary mode (speech) is the streaming-capable branch (binary chunk
        // stream under `stream_format=audio`).
        assert!(!audio_json_mode_is_non_streaming(AudioResponseMode::Binary));
    }

    // ---- S04 build_openai_outbound_body -----------------------------------

    // Helper: build a minimal `LlmRequest` shell with the given payload, model,
    // and stream flag. Mirrors the shape Go's tests assemble via
    // `&llm.Request{Model, Stream, RequestType, APIFormat, <Payload>}`.
    fn llm_request_with_payload(
        payload: LlmRequestPayload,
        model: &str,
        stream: bool,
        request_type: RequestType,
    ) -> LlmRequest {
        LlmRequest {
            request_type,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some(model.to_string()),
            stream,
            payload,
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    // Helper: build a `ChatMessage` with a text content. `ChatMessage` does
    // not derive `Default` (Go's `llm.Message` is a plain struct, so tests
    // there rely on Go zero values); we assemble it with the slots this
    // suite needs.
    fn text_message(role: &str, content: &str) -> conduit_llm::ChatMessage {
        conduit_llm::ChatMessage {
            role: role.to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Text(content.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    // Mirrors Go `RequestFromLLM` (outbound_convert.go) for a chat-completions
    // request: messages / tools / tool_choice round-trip onto the serialized
    // body, and `model` + `stream` are injected from the top-level
    // `LlmRequest` fields. Provider extensions ride via `extra` → top-level
    // JSON keys (Go's typed `Request{}` fields).
    #[test]
    fn s04_chat_payload_serializes_messages_tools_and_injects_model_stream() -> TransformerResult<()>
    {
        let payload = conduit_llm::ChatRequest {
            messages: vec![text_message("user", "hi")],
            temperature: Some(0.7),
            extra: BTreeMap::from([
                ("top_p".to_string(), json!(0.9)),
                ("frequency_penalty".to_string(), json!(0.5)),
            ]),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gpt-4o",
            true,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("gpt-4o")));
        assert_eq!(body.get("stream"), Some(&json!(true)));
        // `messages` is the typed first-class slot. `tool_calls` is a
        // non-skipped `Vec` so it serializes as `[]` even when empty
        // (mirrors Go's `Message.ToolCalls` which omits only on nil, not on
        // empty slice — Go zero-value slice marshals to `[]` too when set).
        assert_eq!(
            body.get("messages"),
            Some(&json!([{"role": "user", "content": "hi", "tool_calls": []}]))
        );
        // `temperature` is a typed first-class slot on `ChatRequest`.
        assert_eq!(body.get("temperature"), Some(&json!(0.7)));
        // `extra` flatten: top_p + frequency_penalty ride as top-level keys.
        assert_eq!(body.get("top_p"), Some(&json!(0.9)));
        assert_eq!(body.get("frequency_penalty"), Some(&json!(0.5)));
        Ok(())
    }

    // Mirrors Go `transformEmbeddingRequest` (embedding.go): the embedding
    // payload (input/encoding_format/dimensions/user) serializes onto the
    // body and `model` is injected from the top-level field; `stream` is
    // hard-false (Go does not carry it on the embedding body).
    #[test]
    fn s04_embedding_payload_serializes_input_and_injects_model() -> TransformerResult<()> {
        let payload = conduit_llm::EmbeddingRequest {
            input: Some(json!("hello world")),
            encoding_format: Some("base64".to_string()),
            dimensions: Some(256),
            user: Some("user-42".to_string()),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Embedding(payload),
            "text-embedding-ada-002",
            false,
            RequestType::Embedding,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("text-embedding-ada-002")));
        assert_eq!(body.get("input"), Some(&json!("hello world")));
        assert_eq!(body.get("encoding_format"), Some(&json!("base64")));
        assert_eq!(body.get("dimensions"), Some(&json!(256)));
        assert_eq!(body.get("user"), Some(&json!("user-42")));
        // Embedding body never carries a top-level `stream` field on the Go
        // side (no `Stream` on `EmbeddingRequest`).
        assert!(body.get("stream").is_none() || body.get("stream") == Some(&json!(false)));
        Ok(())
    }

    // Mirrors Go `buildImageGenerateRequest` (image_outbound.go): the image
    // payload (prompt/n/size/quality/...) serializes onto the body and
    // `model` is injected; `stream` is hard-false.
    #[test]
    fn s04_image_payload_serializes_prompt_and_options() -> TransformerResult<()> {
        let payload = conduit_llm::ImageRequest {
            prompt: Some("a cat".to_string()),
            n: Some(2),
            size: Some("1024x1024".to_string()),
            quality: Some("hd".to_string()),
            response_format: Some("url".to_string()),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Image(payload),
            "dall-e-3",
            false,
            RequestType::Image,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("dall-e-3")));
        assert_eq!(body.get("prompt"), Some(&json!("a cat")));
        assert_eq!(body.get("n"), Some(&json!(2)));
        assert_eq!(body.get("size"), Some(&json!("1024x1024")));
        assert_eq!(body.get("quality"), Some(&json!("hd")));
        assert_eq!(body.get("response_format"), Some(&json!("url")));
        Ok(())
    }

    // Mirrors Go `buildVideoGenerationAPIRequest` (video_outbound.go): the
    // video payload (prompt/image/duration/size) serializes onto the body
    // and `model` is injected; `stream` is hard-false.
    #[test]
    fn s04_video_payload_serializes_prompt_duration_and_size() -> TransformerResult<()> {
        let payload = conduit_llm::VideoRequest {
            prompt: Some("a cat walking".to_string()),
            image: Some(json!("https://example.com/a.png")),
            duration: Some("8".to_string()),
            size: Some("1280x720".to_string()),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Video(payload),
            "sora-2",
            false,
            RequestType::Video,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("sora-2")));
        assert_eq!(body.get("prompt"), Some(&json!("a cat walking")));
        assert_eq!(body.get("image"), Some(&json!("https://example.com/a.png")));
        assert_eq!(body.get("duration"), Some(&json!("8")));
        assert_eq!(body.get("size"), Some(&json!("1280x720")));
        Ok(())
    }

    // Mirrors Go `buildSpeechRequest` (audio_outbound.go): the audio speech
    // payload (input/voice/response_format/...) serializes onto the body and
    // `model` is injected; `stream` is hard-false.
    #[test]
    fn s04_audio_speech_payload_serializes_input_voice_and_options() -> TransformerResult<()> {
        let payload = conduit_llm::AudioRequest {
            input: Some(json!("Hello world")),
            voice: Some("alloy".to_string()),
            response_format: Some("mp3".to_string()),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Audio(payload),
            "tts-1",
            false,
            RequestType::Speech,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("tts-1")));
        assert_eq!(body.get("input"), Some(&json!("Hello world")));
        assert_eq!(body.get("voice"), Some(&json!("alloy")));
        assert_eq!(body.get("response_format"), Some(&json!("mp3")));
        Ok(())
    }

    // Mirrors Go `OutboundTransformer.TransformRequest` model guard
    // (outbound.go:148-150): "model is required".
    #[test]
    fn s04_rejects_missing_model() {
        let request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: None,
            stream: false,
            payload: LlmRequestPayload::Chat(conduit_llm::ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };

        match build_openai_outbound_body(&request) {
            Err(err) => assert!(err.to_string().contains("model is required")),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    // Mirrors Go outbound.go:167 (Compact) + :168-169 (Rerank): the OpenAI
    // outbound transformer rejects these request types with a parity-style
    // error. Compact-on-non-Responses-payload is the S05 guard.
    #[test]
    fn s04_rejects_unsupported_payload_variants() {
        // Rerank — Go rejects it outright.
        let rerank_request = LlmRequest {
            request_type: RequestType::Rerank,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some("rerank-1".to_string()),
            stream: false,
            payload: LlmRequestPayload::Rerank(conduit_llm::RerankRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        match build_openai_outbound_body(&rerank_request) {
            Err(err) => assert!(err.to_string().contains("rerank is not supported")),
            Ok(_) => panic!("expected rerank-not-supported error"),
        }

        // Compact request type on a non-Responses payload — Go's standard
        // outbound.go:166-167 returns "compact is only supported by OpenAI
        // Responses API".
        let compact_on_chat = LlmRequest {
            request_type: RequestType::Compact,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-4.1".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(conduit_llm::ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        match build_openai_outbound_body(&compact_on_chat) {
            Err(err) => assert!(
                err.to_string()
                    .contains("compact is only supported by OpenAI Responses API"),
                "expected compact-only-on-Responses error"
            ),
            Ok(_) => panic!("expected compact-only-on-Responses error"),
        }
    }

    // Mirrors Go's `ExtraBody` merge: top-level provider overrides on
    // `LlmRequest.extra_body` are forwarded onto the outbound JSON body
    // without clobbering explicit typed fields (first-write-wins).
    #[test]
    fn s04_merges_extra_body_without_overwriting_typed_fields() -> TransformerResult<()> {
        let mut request = llm_request_with_payload(
            LlmRequestPayload::Chat(conduit_llm::ChatRequest {
                messages: vec![text_message("user", "hi")],
                temperature: Some(0.5),
                ..Default::default()
            }),
            "gpt-4o",
            false,
            RequestType::Chat,
        );
        // `extra_body` carries both a brand-new key and a key that would
        // clobber `temperature` if second-write-won; first-write-wins must
        // preserve the typed value.
        request.extra_body = BTreeMap::from([
            ("provider_flag".to_string(), json!(true)),
            ("temperature".to_string(), json!(99.0)),
        ]);

        let body = build_openai_outbound_body(&request)?;

        // New key is merged in.
        assert_eq!(body.get("provider_flag"), Some(&json!(true)));
        // Typed temperature (0.5) wins over the extra_body override (99.0).
        assert_eq!(body.get("temperature"), Some(&json!(0.5)));
        Ok(())
    }

    // ---- S05 build_responses_outbound_body (OpenAI Responses outbound) ----

    // Mirrors Go `responses.TransformRequest` (outbound.go:247-272) for the
    // standard Responses body: `model` + `input` + `instructions` +
    // `previous_response_id` + `reasoning` + `tools` + `response_format`
    // round-trip as first-class fields, `stream` is injected from the
    // top-level `LlmRequest`, and the inbound `compact` flag is dropped
    // (Go's `Request{}` has no `compact` field).
    #[test]
    fn s05_standard_responses_body_preserves_typed_fields_and_drops_compact_flag()
    -> TransformerResult<()> {
        let payload = conduit_llm::ResponsesRequest {
            input: Some(json!("summarize this")),
            instructions: Some("keep it short".to_string()),
            previous_response_id: Some("resp_previous".to_string()),
            reasoning: Some(json!({"effort": "low"})),
            tools: vec![conduit_llm::UnifiedTool {
                tool_type: "web_search_preview".to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: BTreeMap::new(),
            }],
            response_format: Some(json!({"type": "json_object"})),
            compact: false,
            extra: BTreeMap::from([
                ("parallel_tool_calls".to_string(), json!(true)),
                ("service_tier".to_string(), json!("priority")),
            ]),
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Responses(payload),
            "gpt-4.1",
            true,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("gpt-4.1")));
        assert_eq!(body.get("input"), Some(&json!("summarize this")));
        assert_eq!(body.get("instructions"), Some(&json!("keep it short")));
        assert_eq!(
            body.get("previous_response_id"),
            Some(&json!("resp_previous"))
        );
        assert_eq!(body.get("reasoning"), Some(&json!({"effort": "low"})));
        assert_eq!(
            body.get("response_format"),
            Some(&json!({"type": "json_object"}))
        );
        assert_eq!(body.get("stream"), Some(&json!(true)));
        // `tools` is a typed first-class slot.
        assert_eq!(
            body.get("tools"),
            Some(&json!([{"type": "web_search_preview"}]))
        );
        // `extra` flatten carries the unmodeled provider fields.
        assert_eq!(body.get("parallel_tool_calls"), Some(&json!(true)));
        assert_eq!(body.get("service_tier"), Some(&json!("priority")));
        // The inbound-routing `compact` flag is dropped from the outbound
        // body (Go's `Request{}` has no such field).
        assert!(body.get("compact").is_none());
        Ok(())
    }

    // Mirrors Go `responses.transformCompactRequest` (compact_outbound.go)
    // building a `CompactAPIRequest{model, input, instructions,
    // prompt_cache_key}`: only those four fields are forwarded, every other
    // field (tools/stream/reasoning/response_format/extras) is dropped.
    #[test]
    fn s05_compact_responses_body_carries_only_four_fields() -> TransformerResult<()> {
        let payload = conduit_llm::ResponsesRequest {
            input: Some(json!([{"role": "user", "content": "hi"}])),
            instructions: Some("be terse".to_string()),
            tools: vec![conduit_llm::UnifiedTool {
                tool_type: "function".to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: BTreeMap::new(),
            }],
            response_format: Some(json!({"type": "json_object"})),
            reasoning: Some(json!({"effort": "low"})),
            compact: true,
            extra: BTreeMap::from([
                ("prompt_cache_key".to_string(), json!("cache-1")),
                ("stream".to_string(), json!(true)),
                ("parallel_tool_calls".to_string(), json!(true)),
            ]),
            ..Default::default()
        };
        // `RequestType::Compact` is legal here because the payload's
        // `compact` flag is set; the S05 guard lets it through.
        let request = llm_request_with_payload(
            LlmRequestPayload::Responses(payload),
            "gpt-4.1",
            true,
            RequestType::Compact,
        );

        let body = build_openai_outbound_body(&request)?;

        // Only the four compact fields are present.
        assert_eq!(body.get("model"), Some(&json!("gpt-4.1")));
        assert_eq!(
            body.get("input"),
            Some(&json!([{"role": "user", "content": "hi"}]))
        );
        assert_eq!(body.get("instructions"), Some(&json!("be terse")));
        assert_eq!(body.get("prompt_cache_key"), Some(&json!("cache-1")));

        // Everything else is dropped (compact API does not accept it).
        assert!(body.get("tools").is_none());
        assert!(body.get("stream").is_none());
        assert!(body.get("reasoning").is_none());
        assert!(body.get("response_format").is_none());
        assert!(body.get("parallel_tool_calls").is_none());
        assert!(body.get("compact").is_none());
        Ok(())
    }

    // Compact body shape with optional fields absent: mirrors Go's `omitempty`
    // semantics — `instructions` and `prompt_cache_key` are omitted when empty.
    #[test]
    fn s05_compact_responses_body_omits_empty_optional_fields() -> TransformerResult<()> {
        let payload = conduit_llm::ResponsesRequest {
            input: Some(json!("hello")),
            compact: true,
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Responses(payload),
            "gpt-4.1",
            false,
            RequestType::Compact,
        );

        let body = build_openai_outbound_body(&request)?;

        assert_eq!(body.get("model"), Some(&json!("gpt-4.1")));
        assert_eq!(body.get("input"), Some(&json!("hello")));
        // Absent optionals are not present on the compact body.
        assert!(body.get("instructions").is_none());
        assert!(body.get("prompt_cache_key").is_none());
        Ok(())
    }

    // Mirrors Go outbound.go:166-167 (standard OpenAI transformer) and
    // outbound.go:197-211 (Responses transformer): a compact request type
    // routed to the standard OpenAI transformer is rejected, but the same
    // request on the Responses path is accepted. The dispatcher here keys on
    // the payload's `compact` flag (the Rust unified model doesn't carry a
    // separate Responses-vs-Chat outbound transformer at this layer; the
    // guard is "compact request type requires a compact-flagged Responses
    // payload").
    #[test]
    fn s05_compact_request_type_requires_compact_responses_payload() {
        // Compact request type + compact-flagged Responses payload → OK (no
        // error from the guard; the body build itself was exercised above).
        let ok_request = LlmRequest {
            request_type: RequestType::Compact,
            api_format: conduit_llm::ApiFormat::OpenAiResponsesCompact,
            model: Some("gpt-4.1".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(conduit_llm::ResponsesRequest {
                input: Some(json!("hi")),
                compact: true,
                ..Default::default()
            }),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        assert!(build_openai_outbound_body(&ok_request).is_ok());

        // Compact request type + non-compact Responses payload → rejected.
        let mismatch_request = LlmRequest {
            request_type: RequestType::Compact,
            api_format: conduit_llm::ApiFormat::OpenAiResponsesCompact,
            model: Some("gpt-4.1".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(conduit_llm::ResponsesRequest {
                input: Some(json!("hi")),
                compact: false,
                ..Default::default()
            }),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };
        match build_openai_outbound_body(&mismatch_request) {
            Err(err) => assert!(
                err.to_string()
                    .contains("compact is only supported by OpenAI Responses API")
            ),
            Ok(_) => panic!("expected compact-only-on-Responses error"),
        }
    }

    // ---- S11 endpoint path override (per-endpoint dispatch) ---------------

    // Mirrors Go's override-or-default idiom shared by every per-endpoint URL
    // builder (outbound.go:412-416, embedding.go:98-102, audio_outbound.go:259-263,
    // image_outbound.go:156-158 / :380-381 / :504-505, responses/outbound.go:326-330):
    // a non-empty `EndpointPath` wins verbatim; otherwise the per-endpoint
    // default path is used.
    #[test]
    fn s11_resolve_outbound_path_override_wins_else_default() {
        // Override set → returned verbatim, regardless of default.
        assert_eq!(
            resolve_outbound_path(Some("/custom/embed"), "/embeddings"),
            "/custom/embed"
        );
        assert_eq!(
            resolve_outbound_path(Some("/v2/chat"), "/chat/completions"),
            "/v2/chat"
        );
        // Override absent → per-endpoint default returned verbatim.
        assert_eq!(resolve_outbound_path(None, "/embeddings"), "/embeddings");
        assert_eq!(
            resolve_outbound_path(None, "/audio/speech"),
            "/audio/speech"
        );
        // Empty / whitespace-only override treated as unset (parity with Go's
        // `!= ""` check applied to a normalized value).
        assert_eq!(
            resolve_outbound_path(Some(""), "/images/generations"),
            "/images/generations"
        );
        assert_eq!(
            resolve_outbound_path(Some("   "), "/images/generations"),
            "/images/generations"
        );
    }

    // Mirrors Go `TestEmbeddingOutboundTransformer_CustomEndpointPath`
    // (embedding_test.go:773-795): with `BaseURL="https://custom.api.com"` and
    // `EndpointPath="/custom/embeddings"`, the embedding URL is
    // `https://custom.api.com/custom/embeddings` — the override replaces the
    // per-endpoint default `/embeddings` AND skips `v1` version appending
    // (NormalizeBaseURL with version="").
    #[test]
    fn s11_endpoint_url_for_embedding_uses_override_and_skips_version() -> TransformerResult<()> {
        let url = resolve_outbound_url_for_endpoint(
            "https://custom.api.com".to_string(),
            Some("/custom/embeddings"),
            false,
            DEFAULT_EMBEDDINGS_PATH,
        )?;
        assert_eq!(url, "https://custom.api.com/custom/embeddings");
        Ok(())
    }

    // Mirrors Go `buildEmbeddingURL` (embedding.go:96-102) default branch:
    // with no override, the embedding URL is `<base>/v1/embeddings`.
    #[test]
    fn s11_endpoint_url_for_embedding_default_appends_v1() -> TransformerResult<()> {
        let url = resolve_outbound_url_for_endpoint(
            "https://api.openai.com".to_string(),
            None,
            false,
            DEFAULT_EMBEDDINGS_PATH,
        )?;
        assert_eq!(url, "https://api.openai.com/v1/embeddings");
        Ok(())
    }

    // Mirrors Go `buildAudioURL("/audio/speech")` (audio_outbound.go:257-264)
    // and the audio speech test `TestOutbound_BuildSpeechRequest`
    // (audio_outbound_test.go:55): default audio speech URL is
    // `<base>/v1/audio/speech`; a channel override replaces it globally.
    #[test]
    fn s11_endpoint_url_for_audio_speech_default_and_override() -> TransformerResult<()> {
        // Default path.
        let default_url = resolve_outbound_url_for_endpoint(
            "https://api.openai.com".to_string(),
            None,
            false,
            DEFAULT_AUDIO_SPEECH_PATH,
        )?;
        assert_eq!(default_url, "https://api.openai.com/v1/audio/speech");

        // Override path replaces the default (channel-global S11 invariant).
        let override_url = resolve_outbound_url_for_endpoint(
            "https://api.openai.com".to_string(),
            Some("/tts/v2"),
            false,
            DEFAULT_AUDIO_SPEECH_PATH,
        )?;
        assert_eq!(override_url, "https://api.openai.com/tts/v2");
        Ok(())
    }

    // Mirrors Go `buildFullRequestURL` RawURL branch (outbound.go:408-410):
    // when RawURL is set, the base is returned verbatim — no version
    // normalization and no per-endpoint path appended, regardless of
    // EndpointPath. This is the RawURL short-circuit every Go per-endpoint
    // builder inherits.
    #[test]
    fn s11_endpoint_url_raw_url_short_circuits_path_append() -> TransformerResult<()> {
        let url = resolve_outbound_url_for_endpoint(
            "https://custom.api.com/full/path/already".to_string(),
            Some("/ignored-override"),
            true,
            DEFAULT_EMBEDDINGS_PATH,
        )?;
        assert_eq!(url, "https://custom.api.com/full/path/already");
        Ok(())
    }

    // ---- S11 model mapping / base_url / override headers ------------------

    // Mirrors Go `ModelMapper.applyModelMapping` golden cases
    // (model_mapper_test.go:13, orchestrator_basic_test.go:533): the first
    // matching rule wins; wildcard, exact, and regex (`*` → all; bare string
    // → exact equality; string with regex chars → anchored regex) all dispatch
    // through `xregexp.MatchString` (internal/pkg/xregexp/match.go:21-39).
    #[test]
    fn s11_map_model_wildcard_exact_and_regex_branches() {
        let mappings = [
            // Exact-match rule (no regex chars → `==`).
            ModelMapping::new("gpt-4o", "provider/gpt4o"),
            // Regex rule (contains `*` regex char) — anchored.
            ModelMapping::new("claude-3.*", "anthropic/claude3"),
            // Wildcard short-circuit (literally `"*"`).
            ModelMapping::new("*", "default/fallback"),
        ];

        // Exact match wins on the first rule.
        assert_eq!(map_model(&mappings, "gpt-4o"), "provider/gpt4o");
        // Regex match (anchored `^(?:claude-3.*)$`).
        assert_eq!(map_model(&mappings, "claude-3-opus"), "anthropic/claude3");
        // Wildcard catches everything else.
        assert_eq!(map_model(&mappings, "gemini-pro"), "default/fallback");

        // Empty mapping list → model returned unchanged
        // (model_mapper.go:157 fallthrough).
        assert_eq!(map_model(&[], "gpt-4o"), "gpt-4o");
    }

    // Mirrors Go `applyModelMapping` no-match fallthrough
    // (model_mapper.go:155-157): when no rule matches, the original model id
    // is returned verbatim, NOT the wildcard. Verify the empty-list and
    // no-match cases both produce the input unchanged.
    #[test]
    fn s11_map_model_returns_original_on_no_match() {
        // List with only non-matching exact rules → input returned.
        let mappings = [ModelMapping::new("gpt-4o", "x")];
        assert_eq!(map_model(&mappings, "claude-3"), "claude-3");

        // Invalid regex pattern never matches (parity with Go's `compileErr`
        // short-circuit, match.go:24-26).
        let bad_regex = [ModelMapping::new("(?P<bad", "x")];
        assert_eq!(map_model(&bad_regex, "anything"), "anything");
    }

    // Mirrors Go `validateConfig` (completion_outbound.go:30-32 /
    // outbound.go:122-124): empty base URL is rejected at the boundary with
    // "base URL is required". The helper is otherwise the identity function
    // (normalization lives in `resolve_outbound_url*`).
    #[test]
    fn s11_apply_outbound_base_url_rejects_empty_returns_unchanged_otherwise()
    -> TransformerResult<()> {
        // Empty / whitespace-only → error.
        let err = apply_outbound_base_url("").err();
        assert_eq!(
            err.as_ref().map(|e| e.error_type()),
            Some("invalid_request")
        );
        assert!(
            err.map(|e| e.public_message().to_lowercase())
                .map_or(false, |m| m.contains("base url"))
        );
        assert!(apply_outbound_base_url("   ").is_err());

        // Non-empty → returned verbatim (no normalization at this layer).
        assert_eq!(
            apply_outbound_base_url("https://api.openai.com/v1/")?,
            "https://api.openai.com/v1/"
        );
        Ok(())
    }

    // Mirrors Go `applyOverrideOperationToHeaders`
    // (internal/server/orchestrator/override.go:394-437): set / delete /
    // rename / copy ops mutate the header map per Go's http.Header semantics
    // (adapted to Rust's single-value BTreeMap). Also exercises the
    // `__CONDUIT_CLEAR__` sentinel (override.go:408-411).
    #[test]
    fn s11_apply_override_header_ops_mirror_go_semantics() {
        let mut headers = HeaderMap::new();
        headers.insert("X-Original".to_string(), "v1".to_string());
        headers.insert("X-Keep".to_string(), "keep".to_string());

        // set: new key.
        apply_override_header_op(&mut headers, &OverrideHeaderOp::set("X-Added"), "new");
        assert_eq!(headers.get("X-Added").map(String::as_str), Some("new"));

        // set: overwrite existing key.
        apply_override_header_op(&mut headers, &OverrideHeaderOp::set("X-Keep"), "updated");
        assert_eq!(headers.get("X-Keep").map(String::as_str), Some("updated"));

        // set with __CONDUIT_CLEAR__ sentinel → delete (override.go:408-411).
        apply_override_header_op(
            &mut headers,
            &OverrideHeaderOp::set("X-Original"),
            CONDUIT_CLEAR_SENTINEL,
        );
        assert!(headers.get("X-Original").is_none());

        // rename: move value from X-Keep → X-Renamed.
        apply_override_header_op(
            &mut headers,
            &OverrideHeaderOp::rename("X-Keep", "X-Renamed"),
            "",
        );
        assert!(headers.get("X-Keep").is_none());
        assert_eq!(
            headers.get("X-Renamed").map(String::as_str),
            Some("updated")
        );

        // rename: missing source → no-op.
        apply_override_header_op(
            &mut headers,
            &OverrideHeaderOp::rename("X-Missing", "X-NoOp"),
            "",
        );
        assert!(headers.get("X-NoOp").is_none());

        // copy: copy X-Renamed → X-Copied, source preserved.
        apply_override_header_op(
            &mut headers,
            &OverrideHeaderOp::copy("X-Renamed", "X-Copied"),
            "",
        );
        assert_eq!(
            headers.get("X-Renamed").map(String::as_str),
            Some("updated")
        );
        assert_eq!(headers.get("X-Copied").map(String::as_str), Some("updated"));

        // delete: drop X-Added.
        apply_override_header_op(&mut headers, &OverrideHeaderOp::delete("X-Added"), "");
        assert!(headers.get("X-Added").is_none());
    }

    // -----------------------------------------------------------------------
    // RUST-P15-001 — outbound_convert_test.go golden cases
    //
    // Mirrors the pure-logic subset of Go's
    // `conduit/llm/transformer/openai/outbound_convert_test.go` (590 lines).
    //
    // Go has explicit type-conversion functions (`RequestFromLLM`,
    // `MessageFromLLM`, `MessageContentPartFromLLM`, `Response.ToLLMResponse`,
    // `Message.ToLLMMessage`, etc.) that convert between the unified `llm.*`
    // types and intermediate `openai.*` model types. The Rust unified
    // architecture serializes the unified types directly to JSON via serde, so
    // there are no intermediate `openai.Request`/`openai.Message` types.
    //
    // The tests below verify that the Rust serde round-trip matches each Go
    // golden case. Where the Rust architecture does not yet replicate a Go
    // filtering/transformation behavior (tool filtering, compaction part
    // filtering, Google thought signature), the test is `#[ignore]`-pinned and
    // documented as a parity gap for Leader triage.
    // -----------------------------------------------------------------------

    use conduit_llm::{
        Annotation, ContentPart, LlmMessage, LlmResponse, MessageContent, OutputAudio, UnifiedTool,
        UrlCitation,
    };

    // ---- TestMessageContentPartAudioRoundTrip (Go L104-124) ---------------
    //
    // Go tests `MessageContentPartFromLLM` + `ToLLMPart` round-trip for an
    // `input_audio` content part. In Rust, `ContentPart` serializes the
    // `input_audio` field directly (serde `#[serde(default,
    // skip_serializing_if = "Option::is_none")]`). The round-trip is exercised
    // via serde_json.

    // Mirrors Go outbound_convert_test.go:104-124 "audio round-trip".
    #[test]
    fn outbound_convert_content_part_audio_round_trip() -> Result<(), serde_json::Error> {
        let part = ContentPart {
            part_type: "input_audio".to_string(),
            text: None,
            image_url: None,
            input_audio: Some(json!({"format": "mp3", "data": "audio-base64"})),
            extra: BTreeMap::new(),
        };
        let wire = serde_json::to_value(&part)?;
        let round_trip: ContentPart = serde_json::from_value(wire.clone())?;

        assert_eq!(round_trip.part_type, "input_audio");
        assert_eq!(
            round_trip.input_audio,
            Some(json!({"format": "mp3", "data": "audio-base64"}))
        );
        // Verify the wire shape matches Go: type + input_audio.
        assert_eq!(wire["type"], "input_audio");
        assert_eq!(wire["input_audio"]["format"], "mp3");
        assert_eq!(wire["input_audio"]["data"], "audio-base64");
        Ok(())
    }

    // ---- TestMessageAudioRoundTrip (Go L196-223) --------------------------
    //
    // Go tests `MessageFromLLM` + `ToLLMMessage` round-trip for a message with
    // `Audio: &llm.OutputAudio{...}`. In Rust, `LlmMessage.audio` is
    // `Option<OutputAudio>` — serde round-trips it natively.

    // Mirrors Go outbound_convert_test.go:196-223 "message audio round-trip".
    #[test]
    fn outbound_convert_message_audio_round_trip() -> Result<(), serde_json::Error> {
        let msg = LlmMessage {
            role: Some("assistant".to_string()),
            content: Some(MessageContent::Text("Audio reply".to_string())),
            audio: Some(OutputAudio {
                id: Some("audio_123".to_string()),
                data: Some("base64-audio".to_string()),
                expires_at: 1234567890,
                transcript: Some("hello world".to_string()),
                extra: BTreeMap::new(),
            }),
            ..Default::default()
        };
        let wire = serde_json::to_value(&msg)?;
        let round_trip: LlmMessage = serde_json::from_value(wire.clone())?;

        let audio = round_trip
            .audio
            .as_ref()
            .ok_or_else(|| serde::de::Error::custom("expected audio field"))?;
        assert_eq!(audio.id.as_deref(), Some("audio_123"));
        assert_eq!(audio.data.as_deref(), Some("base64-audio"));
        assert_eq!(audio.expires_at, 1234567890);
        assert_eq!(audio.transcript.as_deref(), Some("hello world"));

        // Also verify the wire shape.
        assert_eq!(wire["audio"]["id"], "audio_123");
        assert_eq!(wire["audio"]["data"], "base64-audio");
        assert_eq!(wire["audio"]["expires_at"], 1234567890);
        assert_eq!(wire["audio"]["transcript"], "hello world");
        Ok(())
    }

    // ---- TestMessage_ToLLMMessage_WithAnnotations (Go L321-401) -----------
    //
    // Go tests `Message.ToLLMMessage()` with annotations: `url_citation`
    // annotations with `start_index`/`end_index`/`url_citation` round-trip
    // correctly. In Rust, `LlmMessage.annotations: Vec<Annotation>` serde
    // round-trips natively. The Go test also verifies empty annotations → nil;
    // Rust serializes `Vec::new()` as `[]` because `skip_serializing_if =
    // "Vec::is_empty"` is set.

    // Mirrors Go outbound_convert_test.go:328-368 "message with annotations".
    #[test]
    fn outbound_convert_message_annotations_round_trip() -> Result<(), serde_json::Error> {
        let msg = LlmMessage {
            role: Some("assistant".to_string()),
            content: Some(MessageContent::Text("The meaning of life...".to_string())),
            annotations: vec![
                Annotation {
                    annotation_type: Some("url_citation".to_string()),
                    start_index: Some(0),
                    end_index: Some(11),
                    url_citation: Some(UrlCitation {
                        url: Some("https://en.wikipedia.org/wiki/Meaning_of_life".to_string()),
                        title: Some("Meaning of life - Wikipedia".to_string()),
                    }),
                    extra: BTreeMap::new(),
                },
                Annotation {
                    annotation_type: Some("url_citation".to_string()),
                    start_index: Some(20),
                    end_index: Some(27),
                    url_citation: Some(UrlCitation {
                        url: Some("https://plato.stanford.edu/entries/life-meaning/".to_string()),
                        title: Some("The Meaning of Life - Stanford Encyclopedia".to_string()),
                    }),
                    extra: BTreeMap::new(),
                },
            ],
            ..Default::default()
        };
        let wire = serde_json::to_value(&msg)?;
        let round_trip: LlmMessage = serde_json::from_value(wire.clone())?;

        assert_eq!(round_trip.role.as_deref(), Some("assistant"));
        assert_eq!(round_trip.annotations.len(), 2);
        // First annotation.
        assert_eq!(
            round_trip.annotations[0].annotation_type.as_deref(),
            Some("url_citation")
        );
        assert_eq!(round_trip.annotations[0].start_index, Some(0));
        assert_eq!(round_trip.annotations[0].end_index, Some(11));
        let cite0 = round_trip.annotations[0]
            .url_citation
            .as_ref()
            .ok_or_else(|| serde::de::Error::custom("expected url_citation"))?;
        assert_eq!(
            cite0.url.as_deref(),
            Some("https://en.wikipedia.org/wiki/Meaning_of_life")
        );
        assert_eq!(cite0.title.as_deref(), Some("Meaning of life - Wikipedia"));
        // Second annotation.
        assert_eq!(round_trip.annotations[1].start_index, Some(20));
        assert_eq!(round_trip.annotations[1].end_index, Some(27));
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:370-380 "message without annotations"
    // — annotations absent → `LlmMessage.annotations` is empty Vec (serialized
    // as absent due to `skip_serializing_if = "Vec::is_empty"`).
    #[test]
    fn outbound_convert_message_without_annotations_serializes_empty()
    -> Result<(), serde_json::Error> {
        let msg = LlmMessage {
            role: Some("assistant".to_string()),
            content: Some(MessageContent::Text("Hello!".to_string())),
            ..Default::default()
        };
        let wire = serde_json::to_value(&msg)?;
        // `annotations` is `skip_serializing_if = "Vec::is_empty"`, so an empty
        // vec is omitted from the wire JSON (matching Go's nil annotations
        // producing no `annotations` key).
        assert!(
            wire.get("annotations").is_none()
                || wire["annotations"].as_array().is_some_and(Vec::is_empty)
        );
        Ok(())
    }

    // ---- TestResponse_ToLLMResponse (Go L225-319) -------------------------
    //
    // Go tests `Response.ToLLMResponse()` — converts OpenAI `Response` to
    // unified `llm.Response`. In Rust, the unified `LlmResponse` IS the OpenAI
    // response shape, so the "conversion" is a serde round-trip.

    // Mirrors Go outbound_convert_test.go:239-264 "basic response".
    #[test]
    fn outbound_convert_basic_response_round_trip() -> Result<(), serde_json::Error> {
        let response_json = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }]
        });
        let resp: LlmResponse = serde_json::from_value(response_json)?;

        assert_eq!(resp.id, "chatcmpl-123");
        assert_eq!(resp.object, "chat.completion");
        assert_eq!(resp.created, 1677652288);
        assert_eq!(resp.model, "gpt-4");
        assert_eq!(resp.choices.len(), 1);
        let msg = resp.choices[0]
            .message
            .as_ref()
            .ok_or_else(|| serde::de::Error::custom("expected message"))?;
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        match &msg.content {
            Some(MessageContent::Text(s)) if s == "Hello!" => {}
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Text(\"Hello!\"), got {other:?}"
                )));
            }
        }
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:266-287 "streaming response with
    // delta". The OpenAI streaming chunk shape uses `choices[*].delta` instead
    // of `choices[*].message`.
    #[test]
    fn outbound_convert_streaming_response_delta_round_trip() -> Result<(), serde_json::Error> {
        let response_json = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion.chunk",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {"content": "chunk"}
            }]
        });
        let resp: LlmResponse = serde_json::from_value(response_json)?;

        assert_eq!(resp.object, "chat.completion.chunk");
        let delta = resp.choices[0]
            .delta
            .as_ref()
            .ok_or_else(|| serde::de::Error::custom("expected delta"))?;
        match &delta.content {
            Some(MessageContent::Text(s)) if s == "chunk" => {}
            other => {
                return Err(serde::de::Error::custom(format!(
                    "expected Text(\"chunk\"), got {other:?}"
                )));
            }
        }
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:289-309 "response with usage".
    #[test]
    fn outbound_convert_response_with_usage_round_trip() -> Result<(), serde_json::Error> {
        let response_json = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi"}
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let resp: LlmResponse = serde_json::from_value(response_json)?;
        let usage = resp
            .usage
            .as_ref()
            .ok_or_else(|| serde::de::Error::custom("expected usage"))?;
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
        Ok(())
    }

    // ---- TestResponse_ToLLMResponse_WithCitations (Go L403-500) -----------
    //
    // Go stores `citations` (Perplexity-style) in
    // `resp.TransformerMetadata["citations"]`. In Rust, `LlmResponse.extra` or
    // `LlmResponse.transformer_metadata` (both `ExtensionMap`) carry the
    // same data. The stream aggregator (`openai_stream.rs`) already emits
    // citations under `transformer_metadata["citations"]`. Here we verify a
    // non-streaming response JSON carrying top-level `citations` round-trips
    // through `LlmResponse.extra` (via serde flatten).

    // Mirrors Go outbound_convert_test.go:411-441 "response with citations".
    #[test]
    fn outbound_convert_response_citations_round_trip_via_extra() -> Result<(), serde_json::Error> {
        let citations = json!([
            "https://www.theatlantic.com/family/archive/2021/10/meaning-life-macronutrients-purpose-search/620440/",
            "https://en.wikipedia.org/wiki/Meaning_of_life",
            "https://greatergood.berkeley.edu/article/item/three_ways_to_see_meaning_in_your_life"
        ]);
        let response_json = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "llama-3.1-sonar-small-128k-online",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "The meaning of life is..."
                },
                "finish_reason": "stop"
            }],
            "citations": citations
        });
        let resp: LlmResponse = serde_json::from_value(response_json.clone())?;

        // Citations ride via `extra` flatten on `LlmResponse`.
        let got_citations = resp
            .extra
            .get("citations")
            .or_else(|| resp.transformer_metadata.get("citations"))
            .ok_or_else(|| {
                serde::de::Error::custom("expected citations in extra or transformer_metadata")
            })?;

        assert_eq!(got_citations, &response_json["citations"]);
        let arr = got_citations
            .as_array()
            .ok_or_else(|| serde::de::Error::custom("citations should be an array"))?;
        assert_eq!(arr.len(), 3);
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:444-465 "response without citations"
    // — when no citations are present, `TransformerMetadata` should be
    // nil/empty.
    #[test]
    fn outbound_convert_response_without_citations_has_no_metadata() -> Result<(), serde_json::Error>
    {
        let response_json = json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1677652288,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }]
        });
        let resp: LlmResponse = serde_json::from_value(response_json)?;
        assert!(
            resp.transformer_metadata.is_empty(),
            "transformer_metadata should be empty when no citations"
        );
        assert!(
            !resp.extra.contains_key("citations"),
            "extra should not carry citations when absent"
        );
        Ok(())
    }

    // ---- TestRequestFromLLM (Go L13-76) -----------------------------------
    //
    // The Go test has three subtests. The "nil request" case is not
    // expressible in Rust (no nil — `build_openai_outbound_body` takes
    // `&LlmRequest`). The "basic request" case is covered by existing
    // `s04_chat_payload_serializes_messages_tools_and_injects_model_stream`.
    // The "helper fields stripped" case verifies that `MessageIndex` and
    // `APIFormat` (Go llm.Message helper fields) do NOT appear on the OpenAI
    // `Request`. In Rust, `ChatMessage` does not carry `message_index` or
    // `api_format` (those live on `LlmMessage` for the response path), so the
    // outbound body naturally excludes them. We verify that here.

    // Mirrors Go outbound_convert_test.go:49-67 "request with helper fields
    // stripped": `message_index` and `api_format` on the Go `llm.Message` are
    // stripped from the OpenAI request body. In Rust, the request path
    // `ChatMessage` doesn't carry these fields, so the serialized body is
    // clean by construction.
    #[test]
    fn outbound_convert_strips_helper_fields_from_request_body() -> TransformerResult<()> {
        // Set tool_call_id on the message to match Go's test.
        let mut msg = text_message("tool", "result");
        msg.tool_call_id = Some("call_123".to_string());
        let payload = conduit_llm::ChatRequest {
            messages: vec![msg],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gpt-4",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected messages array"))?;
        assert_eq!(messages.len(), 1);
        // tool_call_id IS present (Go preserves it).
        assert_eq!(messages[0]["tool_call_id"], "call_123");
        // message_index and api_format are NOT present on the ChatMessage body
        // (these fields live on LlmMessage for the response path only).
        assert!(messages[0].get("message_index").is_none());
        assert!(messages[0].get("api_format").is_none());
        Ok(())
    }

    // ---- PARITY GAPS (pinned #[ignore] for Leader triage) -----------------
    //
    // The following Go tests exercise filtering/transformation behaviors that
    // the Go `RequestFromLLM` / `MessageContentFromLLM` / `ToolCallFromLLM`
    // apply during outbound conversion. The Rust `build_openai_outbound_body`
    // serializes the unified types directly via serde WITHOUT applying these
    // filters. Each test below documents the expected Go behavior and is
    // `#[ignore]`-pinned until the production filtering is wired.

    // Mirrors Go outbound_convert_test.go:78-102
    // "TestRequestFromLLM_FiltersResponsesCustomTools".
    //
    // Go `RequestFromLLM` (outbound_convert.go:63-65):
    //   req.Tools = lo.FilterMap(r.Tools, func(t llm.Tool, _ int) (Tool, bool) {
    //       return ToolFromLLM(t), t.Type == llm.ToolTypeFunction
    //   })
    //
    // Only `type == "function"` tools survive the outbound conversion. Tools
    // with `type == "responses_custom_tool"` (or any non-function type) are
    // filtered out before serialization. The Rust `build_openai_outbound_body`
    // does NOT apply this filter — it serializes `ChatRequest.tools` verbatim.
    //
    // PARITY GAP: build_openai_outbound_body (openai_outbound.rs:971-978)
    // does not filter tools by type. Go outbound_convert.go:63-65 filters to
    // only `llm.ToolTypeFunction`.
    #[test]
    #[ignore = "PARITY GAP: build_openai_outbound_body does not filter non-function tools (Go outbound_convert.go:63-65). Flagged for Leader."]
    fn outbound_convert_filters_responses_custom_tools() -> TransformerResult<()> {
        let payload = conduit_llm::ChatRequest {
            messages: vec![text_message("user", "hi")],
            tools: vec![
                UnifiedTool {
                    tool_type: "responses_custom_tool".to_string(),
                    name: Some("apply_patch".to_string()),
                    description: None,
                    parameters: None,
                    extra: BTreeMap::new(),
                },
                UnifiedTool {
                    tool_type: "function".to_string(),
                    name: Some("get_weather".to_string()),
                    description: None,
                    parameters: Some(json!({"type": "object"})),
                    extra: BTreeMap::new(),
                },
            ],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gpt-4o",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let tools = body
            .get("tools")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected tools array"))?;
        // Go: only function tools survive → len == 1.
        assert_eq!(tools.len(), 1, "non-function tools should be filtered");
        assert_eq!(tools[0]["type"], "function");
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:126-154
    // "TestMessageContentFromLLM_IgnoresCompactionParts".
    //
    // Go `MessageContentFromLLM` (outbound_convert.go:207-214):
    //   content.MultipleContent = lo.FilterMap(c.MultipleContent, func(p, _) {
    //       switch p.Type {
    //       case "compaction", "compaction_summary": return _, false
    //       default: return _, true
    //       }
    //   })
    //
    // Compaction and compaction_summary content parts are stripped from the
    // outbound message. The Rust `ChatMessage` serializes content parts
    // verbatim via serde — no compaction filtering.
    //
    // PARITY GAP: build_openai_outbound_body (openai_outbound.rs:971-978)
    // does not filter compaction parts. Go outbound_convert.go:207-214 strips
    // them.
    #[test]
    #[ignore = "PARITY GAP: build_openai_outbound_body does not filter compaction/compaction_summary parts (Go outbound_convert.go:207-214). Flagged for Leader."]
    fn outbound_convert_ignores_compaction_parts_in_content() -> TransformerResult<()> {
        let payload = conduit_llm::ChatRequest {
            messages: vec![conduit_llm::ChatMessage {
                role: "assistant".to_string(),
                name: None,
                content: Some(MessageContent::Parts(vec![
                    ContentPart {
                        part_type: "compaction".to_string(),
                        text: None,
                        image_url: None,
                        input_audio: None,
                        extra: BTreeMap::from([
                            ("id".to_string(), json!("cmp_123")),
                            ("encrypted_content".to_string(), json!("secret")),
                        ]),
                    },
                    ContentPart {
                        part_type: "compaction_summary".to_string(),
                        text: None,
                        image_url: None,
                        input_audio: None,
                        extra: BTreeMap::from([
                            ("id".to_string(), json!("cmp_456")),
                            ("encrypted_content".to_string(), json!("summary")),
                        ]),
                    },
                    ContentPart {
                        part_type: "text".to_string(),
                        text: Some("visible".to_string()),
                        image_url: None,
                        input_audio: None,
                        extra: BTreeMap::new(),
                    },
                ])),
                tool_calls: Vec::new(),
                tool_call_id: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gpt-4o",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected messages array"))?;
        let content = messages[0]
            .get("content")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected content array"))?;
        // Go: compaction parts filtered, only "text" survives → len == 1.
        assert_eq!(content.len(), 1, "compaction parts should be filtered");
        assert_eq!(content[0]["type"], "text");
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:156-194
    // "TestRequestFromLLM_IgnoresCompactionPartsInMessages" — same parity gap
    // as above but tested through `RequestFromLLM` at the request level.
    // Same root cause: build_openai_outbound_body does not filter compaction
    // parts. This is the same #[ignore] as the content-level test above;
    // included for completeness of the Go test mapping.
    #[test]
    #[ignore = "PARITY GAP: same as outbound_convert_ignores_compaction_parts_in_content — Go outbound_convert.go:207-214. Flagged for Leader."]
    fn outbound_convert_ignores_compaction_parts_in_messages() -> TransformerResult<()> {
        // Identical root cause to the content-level test above.
        outbound_convert_ignores_compaction_parts_in_content()
    }

    // ---- Google thought signature parity gaps (Go L502-590) ---------------
    //
    // Go `ToolCallFromLLM` (outbound_convert.go:275-281) reads
    // `tc.TransformerMetadata[TransformerMetadataKeyGoogleThoughtSignature]`
    // and, when present and non-empty, wraps it in
    // `ToolCallExtraContent{Google: {ThoughtSignature: raw}}` on the OpenAI
    // `ToolCall`. The Rust unified `ToolCall` has no `extra_content` or
    // `google` typed slot — provider extensions ride via `extra: ExtensionMap`
    // (serde flatten). The Go behavior of injecting `extra_content.google.
    // thought_signature` from `transformer_metadata` is NOT replicated in the
    // Rust outbound serialization.

    // Mirrors Go outbound_convert_test.go:502-533
    // "TestRequestFromLLM_KeepsGoogleThoughtSignatureInRequestModel".
    //
    // Go: a tool call carrying
    // `TransformerMetadata["google_thought_signature"] = "sig_from_metadata"`
    // must produce an OpenAI ToolCall with
    // `ExtraContent.Google.ThoughtSignature = "sig_from_metadata"`. The Rust
    // outbound body does not perform this injection.
    //
    // PARITY GAP: ToolCallFromLLM thought-signature injection
    // (Go outbound_convert.go:275-281) is not implemented in Rust
    // build_openai_outbound_body.
    #[test]
    #[ignore = "PARITY GAP: Google thought signature injection not implemented (Go outbound_convert.go:275-281). Flagged for Leader."]
    fn outbound_convert_keeps_google_thought_signature_in_tool_call() -> TransformerResult<()> {
        let payload = conduit_llm::ChatRequest {
            messages: vec![conduit_llm::ChatMessage {
                role: "assistant".to_string(),
                name: None,
                content: None,
                tool_calls: vec![conduit_llm::ToolCall {
                    id: Some("call_1".to_string()),
                    call_type: "function".to_string(),
                    function: json!({"name": "get_weather", "arguments": "{\"city\":\"Shanghai\"}"}),
                    extra: BTreeMap::from([(
                        "google_thought_signature".to_string(),
                        json!("sig_from_metadata"),
                    )]),
                }],
                tool_call_id: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gemini-3-pro",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let messages = body
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected messages array"))?;
        let tool_calls = messages[0]
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ConduitError::internal("expected tool_calls array"))?;

        // Go: ExtraContent.Google.ThoughtSignature = "sig_from_metadata".
        // The Go wire shape is `extra_content.google.thought_signature`.
        assert_eq!(
            tool_calls[0]["extra_content"]["google"]["thought_signature"],
            "sig_from_metadata"
        );
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:535-569
    // "TestMessageFromLLM_DoesNotOverrideFirstToolCallWhenMetadataExists".
    //
    // Go: when a message has multiple tool calls and the SECOND tool call
    // carries `google_thought_signature` in its TransformerMetadata, the first
    // tool call's ExtraContent must remain nil (no bleed-over).
    //
    // PARITY GAP: same root cause — Google thought signature injection not
    // implemented in Rust outbound.
    #[test]
    #[ignore = "PARITY GAP: Google thought signature per-tool-call handling not implemented (Go outbound_convert.go:275-281). Flagged for Leader."]
    fn outbound_convert_does_not_override_first_tool_call_when_metadata_exists()
    -> TransformerResult<()> {
        let payload = conduit_llm::ChatRequest {
            messages: vec![conduit_llm::ChatMessage {
                role: "assistant".to_string(),
                name: None,
                content: None,
                tool_calls: vec![
                    conduit_llm::ToolCall {
                        id: Some("call_1".to_string()),
                        call_type: "function".to_string(),
                        function: json!({"name": "tool_a", "arguments": "{}"}),
                        extra: BTreeMap::new(),
                    },
                    conduit_llm::ToolCall {
                        id: Some("call_2".to_string()),
                        call_type: "function".to_string(),
                        function: json!({"name": "tool_b", "arguments": "{}"}),
                        extra: BTreeMap::from([(
                            "google_thought_signature".to_string(),
                            json!("sig_from_second_tool_call"),
                        )]),
                    },
                ],
                tool_call_id: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gemini-3-pro",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let tool_calls = body["messages"][0]["tool_calls"]
            .as_array()
            .ok_or_else(|| ConduitError::internal("expected tool_calls array"))?;
        assert_eq!(tool_calls.len(), 2);
        // First tool call: no extra_content (no thought signature).
        assert!(
            tool_calls[0].get("extra_content").is_none()
                || tool_calls[0]["extra_content"].is_null()
        );
        // Second tool call: extra_content.google.thought_signature.
        assert_eq!(
            tool_calls[1]["extra_content"]["google"]["thought_signature"],
            "sig_from_second_tool_call"
        );
        Ok(())
    }

    // Mirrors Go outbound_convert_test.go:571-590
    // "TestMessageFromLLM_GeminiReasoningSignatureDoesNotInjectThoughtSignature".
    //
    // Go: when a message carries `ReasoningSignature` (a Gemini-encoded
    // thought signature on the message, not on a tool call), and a tool call
    // WITHOUT its own `google_thought_signature` metadata, the message-level
    // signature must NOT be injected into the tool call's ExtraContent. Go's
    // `MessageFromLLMWithConfig` only reads tool-call-level
    // TransformerMetadata, not the message-level ReasoningSignature.
    //
    // In Rust, the `reasoning_signature` field lives on `LlmMessage` (response
    // path), not on `ChatMessage` (request path). The request-path message
    // cannot carry `reasoning_signature`, so the injection cannot happen.
    // This test verifies the Rust ChatMessage serialization does NOT produce
    // `extra_content` on tool calls when no `google_thought_signature` is
    // present in the tool call's extra map.
    #[test]
    fn outbound_convert_gemini_reasoning_signature_does_not_inject_thought_signature()
    -> TransformerResult<()> {
        let payload = conduit_llm::ChatRequest {
            messages: vec![conduit_llm::ChatMessage {
                role: "assistant".to_string(),
                name: None,
                content: None,
                tool_calls: vec![conduit_llm::ToolCall {
                    id: Some("call_1".to_string()),
                    call_type: "function".to_string(),
                    function: json!({"name": "tool_a", "arguments": "{}"}),
                    extra: BTreeMap::new(), // no google_thought_signature
                }],
                tool_call_id: None,
                extra: BTreeMap::new(),
            }],
            ..Default::default()
        };
        let request = llm_request_with_payload(
            LlmRequestPayload::Chat(payload),
            "gemini-3-pro",
            false,
            RequestType::Chat,
        );

        let body = build_openai_outbound_body(&request)?;
        let tool_calls = body["messages"][0]["tool_calls"]
            .as_array()
            .ok_or_else(|| ConduitError::internal("expected tool_calls array"))?;
        assert_eq!(tool_calls.len(), 1);
        // No extra_content injected (no google_thought_signature in the tool
        // call's metadata). This mirrors the Go behavior where
        // MessageFromLLMWithConfig does NOT inject from message-level
        // ReasoningSignature.
        assert!(
            tool_calls[0].get("extra_content").is_none()
                || tool_calls[0]["extra_content"].is_null(),
            "tool call should not have extra_content when no google_thought_signature"
        );
        Ok(())
    }
}

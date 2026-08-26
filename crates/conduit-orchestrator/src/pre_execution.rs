//! RUST-P9-006 — orchestrator pre-execution pure-logic helpers (batch 1).
//!
//! Each function in this module mirrors one inbound/outbound middleware from
//! `conduit/internal/server/orchestrator/orchestrator.go` (the
//! `ChatCompletionOrchestrator.Process` middleware chain). The Go middlewares
//! close over runtime collaborators (`*biz.SystemService`,
//! `*PersistentInboundTransformer`, `*PersistentOutboundTransformer`); here we
//! extract the *pure decision logic* so it is unit-testable without those
//! heavy types (which are not yet ported). The orchestrator wiring will call
//! these helpers when RUST-P9-006 S29 lands.
//!
//! Scope of this file (matches TODO_SMALL `[RUST-P9-006]` entries):
//! - **S04** [`strip_billing_header_cch`] — Go `cc.StripBillingHeaderCCH` /
//!   `stripBillingHeaderCCHFromText` (`llm/pipeline/cc/billing_header.go`).
//! - **S07** [`apply_auto_reasoning_effort`] / [`split_auto_reasoning_effort_model`]
//!   — Go `applyAutoReasoningEffort` / `splitAutoReasoningEffortModel`
//!   (`internal/server/orchestrator/auto_reasoning_effort.go`).
//! - **S08** [`check_api_key_model_access`] — Go `checkApiKeyModelAccess`
//!   (`internal/server/orchestrator/model_access.go`).
//! - **S09** [`apply_model_mapping`] / [`ModelMapper`] — Go `applyModelMapping` +
//!   `ModelMapper.MapModel` / `applyModelMapping` / `matchesMapping`
//!   (`internal/server/orchestrator/model_mapper.go`). NOTE: this is the
//!   **API-key profile** model mapping (`ModelMapping{From,To}` regex), NOT the
//!   channel `extra_model_prefix`/`auto_trimed_model_prefixes`/`hide_*`/`lowercase`
//!   settings (those are applied at channel-resolution time in
//!   `conduit-services::channel_service`).
//! - **S18** [`apply_user_agent_pass_through`] — Go `applyUserAgentPassThrough`
//!   (`internal/server/orchestrator/pass_through.go`).
//! - **S19** [`apply_override_request_headers`] /
//!   [`apply_override_operation_to_headers`] — Go `applyOverrideRequestHeaders` +
//!   `applyOverrideOperationToHeaders` (`internal/server/orchestrator/override.go`).
//!
//! Go-parity doubts and blockers are flagged inline with `[Pascal ?]`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use conduit_core::error::{ConduitError, ErrorKind};
use conduit_core::objects::channel_settings::{ChannelSettings, ModelMapping};
use conduit_core::objects::overrides::{OverrideOperation, override_op};
use conduit_llm::{
    ChatMessage, ChatRequest, HttpRequest, LlmRequest, LlmRequestPayload, MessageContent,
};
use regex::Regex;
use serde_json::Value;

// ---------------------------------------------------------------------------
// LlmRequest Chat-payload helpers (private)
// ---------------------------------------------------------------------------

/// Private accessor that mirrors Go's free-form mutation of
/// `llm.Request.ReasoningEffort` (a top-level string in Go, but in Rust the
/// field lives on the chat payload). If the payload is not the chat variant,
/// it is coerced to a default chat payload to preserve Go's "always writable"
/// semantics.
fn chat_payload_or_default_mut(request: &mut LlmRequest) -> &mut ChatRequest {
    if !matches!(request.payload, LlmRequestPayload::Chat(_)) {
        request.payload = LlmRequestPayload::Chat(ChatRequest::default());
    }
    match &mut request.payload {
        LlmRequestPayload::Chat(chat) => chat,
        _ => unreachable!("payload was just forced to Chat"),
    }
}

/// Private read-side accessor mirroring Go's free-form read of
/// `llm.Request.ReasoningEffort` from the chat payload.
fn chat_payload_ref(payload: &LlmRequestPayload) -> Option<&ChatRequest> {
    match payload {
        LlmRequestPayload::Chat(chat) => Some(chat),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// S04 — StripBillingHeaderCCH (Go: `llm/pipeline/cc/billing_header.go`)
// ---------------------------------------------------------------------------

/// Go: `billingHeaderPrefix = "x-anthropic-billing-header:"`.
pub const BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";

/// Go: `BillingCCHKey = "claudecode_billing_cch"`. Key under which the stripped
/// cch value is stored in the request's transformer metadata.
pub const BILLING_CCH_KEY: &str = "claudecode_billing_cch";

/// Output of [`strip_billing_header_cch`]. Mirrors the Go middleware's mutation
/// of `request.Messages[*].Content` plus the captured cch value written into
/// `request.TransformerMetadata[BillingCCHKey]`.
///
/// In Go the captured cch is written into the same `*llm.Request` it mutated.
/// Rust's [`LlmRequest`] does not yet carry a `transformer_metadata` map (the
/// Go `llm.Request.TransformerMetadata` field is absent from the Rust port), so
/// we surface the captured value as a side output. The orchestrator wiring
/// stores it under the inbound request's `HttpRequest.transformer_metadata`
/// (which IS ported) using [`BILLING_CCH_KEY`]. `[Pascal ?]`: verify the wiring
/// point matches Go once `PersistentInboundTransformer` lands.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct BillingCchOutcome {
    /// The captured cch value, or empty when nothing was stripped.
    pub captured_cch: String,
    /// `true` if any message text was rewritten.
    pub changed: bool,
}

/// S04 — Strip the random `cch=...;` suffix from Claude Code's
/// `x-anthropic-billing-header:` system message(s) in place, and return the
/// captured cch value (Go `cc.StripBillingHeaderCCH().OnInboundLlmRequest`).
///
/// Behavior faithfully mirrors `conduit/llm/pipeline/cc/billing_header.go`:
/// - iterates `request.messages`, only `role == "system"` messages are touched;
/// - for `MessageContent::Text` and each `text`-typed part of
///   `MessageContent::Parts`, the inner text is rewritten by
///   [`strip_billing_header_cch_from_text`];
/// - the first captured cch value wins; subsequent ones are ignored;
/// - `MessageContent::Json` is left untouched (Go only inspects
///   `Content.Content` and `Content.MultipleContent[*].Text`, which the Rust
///   enum models as `Text` / `Parts`).
pub fn strip_billing_header_cch(request: &mut LlmRequest) -> BillingCchOutcome {
    let mut outcome = BillingCchOutcome::default();

    let LlmRequestPayload::Chat(chat) = &mut request.payload else {
        return outcome;
    };

    for message in &mut chat.messages {
        if message.role != "system" {
            continue;
        }

        let Some(content) = message.content.as_mut() else {
            continue;
        };

        match content {
            MessageContent::Text(text) => {
                let (new_text, cch, did_change) = strip_billing_header_cch_from_text(text);
                if did_change {
                    outcome.changed = true;
                    *text = new_text;
                    if outcome.captured_cch.is_empty() && !cch.is_empty() {
                        outcome.captured_cch = cch;
                    }
                }
            }
            MessageContent::Parts(parts) => {
                for part in parts.iter_mut() {
                    if part.part_type != "text" {
                        continue;
                    }
                    let Some(text) = part.text.as_mut() else {
                        continue;
                    };
                    let (new_text, cch, did_change) = strip_billing_header_cch_from_text(text);
                    if did_change {
                        outcome.changed = true;
                        *text = new_text;
                        if outcome.captured_cch.is_empty() && !cch.is_empty() {
                            outcome.captured_cch = cch;
                        }
                    }
                }
            }
            MessageContent::Json(_) => {
                // Go only touches `Content.Content` / `Content.MultipleContent[*].Text`.
                // The Rust `Json` variant has no direct Go counterpart to walk here.
            }
        }
    }

    outcome
}

/// Strip the `cch=...;` segment from one billing-header text and return
/// `(new_text, cch_value, changed)`. Faithful port of Go
/// `stripBillingHeaderCCHFromText`.
///
/// Algorithm (mirror exactly):
/// 1. trim, lowercase-prefix check against [`BILLING_HEADER_PREFIX`];
/// 2. split the remainder on `;`, drop empty segments and the first `cch=`
///    segment (its value is captured);
/// 3. rebuild as `"<prefix> <kept>; "` joined by `"; "`, with a trailing `;`
///    when the original had one or any segment survived;
/// 4. if no `cch=` segment was present, return unchanged.
pub fn strip_billing_header_cch_from_text(text: &str) -> (String, String, bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (text.to_string(), String::new(), false);
    }

    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with(BILLING_HEADER_PREFIX) {
        return (text.to_string(), String::new(), false);
    }

    let rest = trimmed[BILLING_HEADER_PREFIX.len()..].trim();
    if rest.is_empty() {
        return (text.to_string(), String::new(), false);
    }

    // Go checks the trailing-semicolon status on the *trimmed* remainder (after
    // stripping the prefix); preserve that exactly.
    let original_had_trailing_semi = rest.trim().ends_with(';');

    let parts: Vec<&str> = rest.split(';').collect();
    let mut kept: Vec<String> = Vec::with_capacity(parts.len());
    let mut cch = String::new();

    for raw in parts {
        let segment = raw.trim();
        if segment.is_empty() {
            continue;
        }

        let segment_lower = segment.to_ascii_lowercase();
        if segment_lower.starts_with("cch=") {
            if cch.is_empty() {
                cch = segment["cch=".len()..].trim().to_string();
            }
            continue;
        }

        kept.push(segment.to_string());
    }

    if cch.is_empty() {
        return (text.to_string(), String::new(), false);
    }

    // Rebuild: "<prefix> <k1>; <k2>" then append a trailing ';' if the original
    // had one OR at least one segment survived.
    let joined = kept.join("; ");
    let mut out = format!("{BILLING_HEADER_PREFIX} {joined}");
    if original_had_trailing_semi || !kept.is_empty() {
        out = format!("{};", out.trim_end());
    }

    (out, cch, true)
}

// ---------------------------------------------------------------------------
// S07 — applyAutoReasoningEffort (Go: `auto_reasoning_effort.go`)
// ---------------------------------------------------------------------------

/// Go: `supportedAutoReasoningEfforts` — model id suffixes that are interpreted
/// as a reasoning-effort directive when `SystemModelSettings.auto_reasoning_effort`
/// is enabled. Note `max` is in the set but is excluded for Qwen models.
pub const SUPPORTED_AUTO_REASONING_EFFORTS: &[&str] = &["max", "xhigh", "high", "medium", "low"];

/// S07 — Split a model id into `(base_model, reasoning_effort)` if its last
/// `-`-separated suffix is a supported effort token (Go
/// `splitAutoReasoningEffortModel`).
///
/// Parity details:
/// - the suffix is matched case-insensitively (Go lowercases);
/// - `max` is rejected for Qwen models via [`is_qwen_max_model`] (Go preserves
///   `qwen3.7-max` / `qwen/qwen3-max` as-is);
/// - returns `None` when there is no `-`, the suffix is unsupported, or the
///   base would be empty (Go: `lastDash <= 0`).
pub fn split_auto_reasoning_effort_model(model: &str) -> Option<(String, String)> {
    let last_dash = match model.rfind('-') {
        Some(index) if index > 0 && index < model.len() - 1 => index,
        _ => return None,
    };

    let effort = model[last_dash + 1..].to_ascii_lowercase();
    if !SUPPORTED_AUTO_REASONING_EFFORTS.contains(&effort.as_str()) {
        return None;
    }

    if effort == "max" && is_qwen_max_model(model) {
        return None;
    }

    let base = &model[..last_dash];
    if base.is_empty() {
        return None;
    }

    Some((base.to_string(), effort))
}

/// Go `isQwenMaxModel`: returns true when the lowercased model id ends in
/// `-max` and its last `/`-separated segment starts with `qwen`. Used by S07 to
/// keep `qwen3.7-max` / `qwen/qwen3-max` intact (their `-max` is part of the
/// model name, not an effort directive).
pub fn is_qwen_max_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    if !normalized.ends_with("-max") {
        return false;
    }

    // Go takes the segment after the last `/`.
    let last_segment = match normalized.rfind('/') {
        Some(slash) => &normalized[slash + 1..],
        None => &normalized,
    };

    last_segment.starts_with("qwen")
}

/// S07 — Apply auto-reasoning-effort to an [`LlmRequest`] in place (Go
/// `applyAutoReasoningEffort` middleware).
///
/// Behavior:
/// - if `settings.auto_reasoning_effort` is disabled, or the request has no
///   model, do nothing;
/// - otherwise split the model via [`split_auto_reasoning_effort_model`]; if a
///   split succeeds, rewrite `request.model` to the base and write the effort
///   to the chat payload's `reasoning_effort` (Go `llm.Request.ReasoningEffort`
///   is a top-level string; in Rust it lives on `ChatRequest`).
///
/// `[Pascal ?]`: Rust models `reasoning_effort` under `ChatRequest`, not as a
/// top-level `LlmRequest` field like Go. This helper writes through
/// `chat_payload_or_default_mut(&mut request).reasoning_effort`. If the payload is
/// not the Chat variant, the effort is still recorded on the chat default
/// payload to preserve Go semantics (Go does not check the payload kind).
pub fn apply_auto_reasoning_effort(
    request: &mut LlmRequest,
    settings: &conduit_core::objects::SystemModelSettings,
) {
    let Some(model) = request.model.clone() else {
        return;
    };
    if model.is_empty() {
        return;
    }
    if !settings.auto_reasoning_effort {
        return;
    }

    let Some((base, effort)) = split_auto_reasoning_effort_model(&model) else {
        return;
    };

    request.model = Some(base);
    chat_payload_or_default_mut(request).reasoning_effort = Some(effort);
}

// ---------------------------------------------------------------------------
// S08 — checkApiKeyModelAccess (Go: `model_access.go`)
// ---------------------------------------------------------------------------

/// Minimal typed view of the API key + its active profile, for S08/S09. The Go
/// middleware reads `inbound.state.APIKey` (`*ent.APIKey`), which is not ported
/// yet; this trait boundary keeps the pure logic testable. `[Pascal ?]`: replace
/// with the real `ApiKey` snapshot once `ent.APIKey` lands.
pub trait ApiKeyProfileView: Send + Sync {
    /// The API key display name (Go `apiKey.Name`) — used only for diagnostics.
    fn name(&self) -> &str;
    /// The active profile name (Go `apiKey.Profiles.ActiveProfile`).
    fn active_profile_name(&self) -> &str;
    /// The active profile's explicit model allow-list (Go
    /// `profile.ModelIDs`). Empty means "all models allowed".
    fn model_ids(&self) -> &[String];
    /// The active profile's model mappings (Go `profile.ModelMappings`).
    fn model_mappings(&self) -> &[ModelMapping];
}

/// S08 — Decide whether `model` is allowed by the API key's active profile.
///
/// Returns:
/// - `Ok(())` when there is no API key, no active profile, the profile has an
///   empty `ModelIDs` list, or `model` is in the list (Go `slices.Contains`);
/// - `Err(ConduitError::InvalidModel)` with `": <model>"` appended when access is
///   denied (Go `fmt.Errorf("%w: %s", biz.ErrInvalidModel, model)`); or with
///   `": request model is empty"` when `model` is empty (Go aborts with the
///   same wrapping).
///
/// `api_key` is `Option` to mirror Go's `inbound.state.APIKey == nil` shortcut.
pub fn check_api_key_model_access(
    model: &str,
    api_key: Option<&dyn ApiKeyProfileView>,
) -> Result<(), ConduitError> {
    if model.is_empty() {
        // Go: `fmt.Errorf("%w: request model is empty", biz.ErrInvalidModel)`.
        return Err(invalid_model("request model is empty"));
    }

    let Some(api_key) = api_key else {
        return Ok(());
    };

    // Go resolves the active profile via `apiKey.GetActiveProfile()`. Our view
    // already exposes the active profile's lists directly.
    let allowed_models = api_key.model_ids();
    if allowed_models.is_empty() {
        return Ok(());
    }

    if allowed_models.iter().any(|m| m == model) {
        Ok(())
    } else {
        Err(invalid_model(model))
    }
}

/// Build an `ConduitError` of kind `InvalidModel` whose message mirrors Go's
/// `fmt.Errorf("%w: %s", biz.ErrInvalidModel, detail)`. The Go wrapping keeps
/// the sentinel error's default safe message (`"Invalid model"`) but enriches
/// the diagnostic message; we mirror that by setting both the message and the
/// safe message to `"Invalid model: <detail>"`.
fn invalid_model(detail: &str) -> ConduitError {
    let message = format!("Invalid model: {detail}");
    ConduitError::new(ErrorKind::InvalidModel, message.clone()).with_safe_message(message)
}

// ---------------------------------------------------------------------------
// S09 — applyModelMapping (Go: `model_mapper.go`)
// ---------------------------------------------------------------------------

/// S09 — Pure model-mapping decision used by the orchestrator's API-key profile
/// mapping step. Mirrors Go `(*ModelMapper).MapModel` +
/// `(*ModelMapper).applyModelMapping` + `(*ModelMapper).matchesMapping`.
///
/// The Go `matchesMapping` calls `xregexp.MatchString(pattern, model)` which
/// has three branches (see `conduit/internal/pkg/xregexp/match.go`):
/// 1. pattern `"*"` -> matches everything;
/// 2. pattern without regex metacharacters -> exact string equality;
/// 3. otherwise -> anchored regex (`^(?:body)$`, case-sensitive, no flags).
///
/// This port reproduces all three branches. `[Pascal ?]`: Go uses
/// `dlclark/regexp2` which supports look-around and other PCRE features the
/// `regex` crate does not. For the patterns that fail to compile under Rust's
/// RE2-style engine we return `false` (matching Go's `compileErr` behavior),
/// but a pattern relying on look-around would silently mismatch — flagged for
/// the parity auditor.
#[derive(Debug, Default, Clone, Copy)]
pub struct ModelMapper;

impl ModelMapper {
    /// Apply `mappings` to `model`, returning the `to` of the first matching
    /// mapping, or `model` unchanged when no mapping matches. Mirrors Go
    /// `(*ModelMapper).applyModelMapping`. Returns an owned `String` (the
    /// result is either one of the `mapping.to` strings or the input).
    pub fn map_model_owned(mappings: &[ModelMapping], model: &str) -> String {
        for mapping in mappings {
            if Self::matches_mapping(&mapping.from, model) {
                return mapping.to.clone();
            }
        }
        model.to_string()
    }

    /// Go `(*ModelMapper).matchesMapping` -> `xregexp.MatchString(pattern, model)`.
    pub fn matches_mapping(pattern: &str, model: &str) -> bool {
        xregexp_match_string(pattern, model)
    }
}

/// Faithful port of Go `xregexp.MatchString(pattern, str)` (see
/// `conduit/internal/pkg/xregexp/match.go`). Branches:
/// 1. `pattern == "*"` -> `true`;
/// 2. pattern has no regex metacharacters -> exact equality with `str`;
/// 3. otherwise anchored regex `^(?:<body>)$`; compile failure -> `false`.
pub fn xregexp_match_string(pattern: &str, str: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    if !contains_regex_chars(pattern) {
        return pattern == str;
    }

    // Strip an optional inline `(?...)` modifier (Go `splitInlineModifier`).
    let (modifier, body) = split_inline_modifier(pattern);
    let body = body.strip_prefix('^').unwrap_or(body);
    let body = body.strip_suffix('$').unwrap_or(body);

    let anchored = format!("{modifier}^(?:{body})$");

    match Regex::new(&anchored) {
        Ok(re) => re.is_match(str),
        Err(_) => false,
    }
}

/// Go `containsRegexChars`: pattern has any of the regex metacharacters that
/// trigger the regex branch.
fn contains_regex_chars(pattern: &str) -> bool {
    pattern.chars().any(|c| {
        matches!(
            c,
            '*' | '?' | '+' | '[' | ']' | '{' | '}' | '(' | ')' | '^' | '$' | '.' | '|' | '\\'
        )
    })
}

/// Go `splitInlineModifier`: if the pattern starts with `(?...)`, split off the
/// inline modifier prefix; otherwise `(empty, pattern)`. Mirrors the Go
/// early-outs (modifier must contain only flag chars, not `:=!<`).
fn split_inline_modifier(pattern: &str) -> (&str, &str) {
    if !pattern.starts_with("(?") {
        return ("", pattern);
    }

    let Some(end) = pattern.find(')') else {
        return ("", pattern);
    };
    if end <= 2 {
        return ("", pattern);
    }

    let modifier = &pattern[..=end];
    let body = &pattern[end + 1..];

    // Go: if the modifier (between `(?` and `)`) contains any of `:=!<`, treat
    // the whole thing as a non-modifier pattern.
    let inner = &pattern[2..end];
    if inner.chars().any(|c| matches!(c, ':' | '=' | '!' | '<')) {
        return ("", pattern);
    }

    (modifier, body)
}

/// S09 — Orchestrator-level entry point mirroring Go `applyModelMapping`
/// middleware's body: record the original client model and apply the API-key
/// profile's `ModelMappings` to `request.model`.
///
/// Returns the original (pre-mapping) model so the caller (the future
/// `PersistentInboundTransformer`) can stash it for the outbound transformer's
/// response-model restoration (Go stores it on
/// `m.RequestModel` / `state.OriginalModel`).
///
/// `api_key` is `Option` to mirror Go's nil-shortcut.
pub fn apply_model_mapping(
    request: &mut LlmRequest,
    api_key: Option<&dyn ApiKeyProfileView>,
) -> Result<String, ConduitError> {
    let original = match request.model.as_deref() {
        Some(model) if !model.is_empty() => model.to_string(),
        _ => return Err(invalid_model("request model is empty")),
    };

    let Some(api_key) = api_key else {
        return Ok(original);
    };

    let mappings = api_key.model_mappings();
    if mappings.is_empty() {
        return Ok(original);
    }

    let mapped = ModelMapper::map_model_owned(mappings, &original);
    if mapped != original {
        request.model = Some(mapped);
    }

    Ok(original)
}

// ---------------------------------------------------------------------------
// S18 — applyUserAgentPassThrough (Go: `pass_through.go`)
// ---------------------------------------------------------------------------

/// Default Conduit API `User-Agent` literal (Go `pass_through.go`).
pub const DEFAULT_USER_AGENT: &str = "conduit/1.0";

/// S18 — Decide the `User-Agent` header value for an outbound raw request,
/// mirroring Go `applyUserAgentPassThrough.OnRawRequest`.
///
/// Inputs:
/// - `channel_pass_through`: the channel-level override (`channel.Settings.pass_through_user_agent`); `None` means "fall back to global".
/// - `global_pass_through`: the system-level setting (Go
///   `SystemService.UserAgentPassThrough(ctx)`).
/// - `client_user_agent`: the original client `User-Agent` header (Go reads it
///   from `outbound.state.LlmRequest.RawRequest.Headers`).
///
/// Output:
/// - pass-through enabled AND a non-empty client UA -> the client UA;
/// - pass-through enabled but no client UA -> fall back to `conduit/1.0` (Go
///   only overwrites when the client UA is non-empty);
/// - pass-through disabled -> `"conduit/1.0"` (Go's hardcoded default).
pub fn decide_user_agent(
    channel_pass_through: Option<bool>,
    global_pass_through: bool,
    client_user_agent: Option<&str>,
) -> String {
    let enabled = channel_pass_through.unwrap_or(global_pass_through);

    if enabled
        && let Some(ua) = client_user_agent
        && !ua.is_empty()
    {
        return ua.to_string();
    }

    // Go default for both "disabled" and "enabled-but-no-client-UA".
    DEFAULT_USER_AGENT.to_string()
}

/// S18 — Apply the user-agent decision to an outbound [`HttpRequest`] in place
/// (Go mutates `request.Headers`). Returns the value that was written.
pub fn apply_user_agent_pass_through(
    request: &mut HttpRequest,
    channel_pass_through: Option<bool>,
    global_pass_through: bool,
    client_user_agent: Option<&str>,
) -> String {
    let value = decide_user_agent(channel_pass_through, global_pass_through, client_user_agent);
    request
        .headers
        .insert("User-Agent".to_string(), value.clone());
    value
}

// ---------------------------------------------------------------------------
// S19 — applyOverrideRequestHeaders (Go: `override.go`)
// ---------------------------------------------------------------------------

/// Render context for override templates. Mirrors Go `RenderContext` (Go struct
/// in `override.go`). Used by [`apply_override_operation_to_headers`] /
/// [`render_template`].
///
/// `[Pascal ?]`: the Go `text/template` engine supports the full `{{.Field}}`
/// / `{{index .Map "k"}}` / pipeline / conditional grammar. Our
/// [`render_template`] port implements only the subset the orchestrator tests
/// exercise: `{{.Field}}` access on top-level string fields, and literal
/// pass-through. Complex templates will be returned unchanged (Go returns the
/// original on parse/exec error too). Flagged for the parity auditor.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RenderContext {
    /// Go `.RequestModel` — the original client model (pre any mapping).
    pub request_model: String,
    /// Go `.Model` — the model currently on the outbound LlmRequest.
    pub model: String,
    /// Go `.Metadata` — keyed string metadata. Sourced from
    /// `LlmRequest.metadata`; Go types it as `map[string]string`.
    pub metadata: BTreeMap<String, String>,
    /// Go `.RequestHeader` — filtered client request headers (canonical +
    /// lower-cased keys, sensitive headers excluded).
    pub request_header: BTreeMap<String, String>,
    /// Go `.ReasoningEffort` — the current reasoning effort directive.
    pub reasoning_effort: String,
}

/// Build the render context from the outbound [`LlmRequest`] snapshot, mirroring
/// Go `buildRenderContext(llmReq, requestModel)` + `buildRequestHeaderMap`.
///
/// Sensitive header filtering follows Go `httpclient.IsSensitiveHeader`
/// (`authorization`, `x-api-key`, `x-goog-api-key`, `cookie`).
pub fn build_render_context(
    llm_request: Option<&LlmRequest>,
    request_model: &str,
) -> RenderContext {
    let Some(req) = llm_request else {
        return RenderContext {
            request_model: request_model.to_string(),
            ..Default::default()
        };
    };

    let model = req.model.clone().unwrap_or_default();
    let reasoning_effort = chat_payload_ref(&req.payload)
        .and_then(|c| c.reasoning_effort.clone())
        .unwrap_or_default();

    // Go reads `llmReq.Metadata` which is `map[string]string`. Our Rust
    // `LlmRequest.metadata` is `ExtensionMap = BTreeMap<String, Value>`; coerce
    // to strings.
    let metadata = string_metadata(&req.metadata);

    // `[Pascal ?]`: Go reads `llmReq.RawRequest.Headers` here, but Rust's
    // `LlmRequest` has no `raw_request` field (only `HttpRequest` does). Callers
    // pass the raw client request separately; for now the request-header map is
    // built from `extra_headers` (the Rust shape closest to Go's `RawRequest.Headers`).
    let request_header = build_request_header_map_from_header_map(&req.extra_headers);

    RenderContext {
        request_model: request_model.to_string(),
        model,
        metadata,
        request_header,
        reasoning_effort,
    }
}

/// Convert the JSON-typed `metadata` map to `String -> String` (Go types it as
/// `map[string]string`). Non-string JSON values are stringified via
/// `serde_json::Value`'s `Display` (mirrors Go's `fmt.Sprintf("%v", v)` for the
/// legacy parse path — see `conduit-core::objects::overrides`).
fn string_metadata(metadata: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    metadata
        .iter()
        .map(|(k, v)| (k.clone(), value_to_string(v)))
        .collect()
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "<nil>".to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Build the `RequestHeader` map (canonical + lower-cased keys, sensitive
/// headers excluded). Mirrors Go `buildRequestHeaderMap`. Accepts the
/// JSON-typed raw-request snapshot (Go `*httpclient.Request` serialized).
pub fn build_request_header_map(raw_request: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(Value::Object(headers)) = raw_request.and_then(|v| v.get("headers")) else {
        return out;
    };

    for (key, value) in headers {
        if is_sensitive_header(key) {
            continue;
        }

        // Go takes `values[0]`. The raw_request snapshot can be string- or
        // array-typed; pick the first value if possible.
        let first = match first_header_value(value) {
            Some(v) => v,
            None => continue,
        };

        out.insert(canonical_header_key(key), first.clone());
        out.insert(key.to_ascii_lowercase(), first);
    }

    out
}

/// Build the `RequestHeader` map from a Rust `HeaderMap`
/// (`BTreeMap<String, String>`). Same filtering as
/// [`build_request_header_map`] but operates on the already-typed single-value
/// header map the Rust port uses.
pub fn build_request_header_map_from_header_map(
    headers: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (key, value) in headers {
        if is_sensitive_header(key) {
            continue;
        }
        out.insert(canonical_header_key(key), value.clone());
        out.insert(key.to_ascii_lowercase(), value.clone());
    }
    out
}

/// Take the first header value from a JSON-typed header (string or array of
/// strings). Mirrors Go `values[0]`.
fn first_header_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => items.first().and_then(Value::as_str).map(String::from),
        _ => None,
    }
}

/// Canonicalize a header key (Go `http.CanonicalHeaderKey`). The full HTTP
/// canonicalization is involved; we implement the common case (title-case each
/// `-`-separated segment) which matches every header the Go tests touch.
fn canonical_header_key(key: &str) -> String {
    key.split('-')
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    let tail: String = chars.collect();
                    first.to_ascii_uppercase().to_string() + &tail.to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Sensitive header predicate — mirrors Go `httpclient.IsSensitiveHeader`
/// (lowercased match against `authorization | x-api-key | x-goog-api-key | cookie`).
fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "x-goog-api-key" | "cookie"
    )
}

/// Evaluate a Go `text/template` expression against [`RenderContext`] with the
/// same fallback semantics as Go's `renderTemplate`: on parse or execute error,
/// return the original `value`.
///
/// **Parity caveat ([Pascal ?]):** only the `{{.Field}}` subset is implemented.
/// Anything else (conditionals, ranges, `index`, pipelines, custom funcs) is
/// returned unchanged. The Go orchestrator tests for header overrides
/// (`override_test.go`) use only `{{.Model}}` for headers, so this covers S19's
/// golden cases; S17 (body override) and other templates will need a fuller
/// engine.
pub fn render_template(value: &str, ctx: &RenderContext) -> String {
    // Fast path: Go short-circuits when there is no `{{...}}` pair.
    if !(value.contains("{{") && value.contains("}}")) {
        return value.to_string();
    }

    render_simple_template(value, ctx)
}

/// Walk the template string and substitute `{{.Field}}` tokens with the
/// matching [`RenderContext`] field. Unknown fields leave the token intact.
fn render_simple_template(template: &str, ctx: &RenderContext) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        match after_open.find("}}") {
            Some(close) => {
                let expr = after_open[..close].trim();
                out.push_str(&resolve_template_expr(expr, ctx));
                rest = &after_open[close + 2..];
            }
            None => {
                // Unbalanced `{{` — keep the rest verbatim (Go would parse-error
                // and return the original).
                out.push_str("{{");
                rest = after_open;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Resolve a single `{{...}}` expression to a string. Only `.Field` is
/// supported; anything else (including `index`, pipelines, conditionals)
/// returns the original `{{expr}}` text unchanged.
fn resolve_template_expr(expr: &str, ctx: &RenderContext) -> String {
    let Some(field) = expr.strip_prefix('.') else {
        return format!("{{{{{expr}}}}}");
    };

    match field {
        "RequestModel" | "request_model" => ctx.request_model.clone(),
        "Model" | "model" => ctx.model.clone(),
        "ReasoningEffort" | "reasoning_effort" => ctx.reasoning_effort.clone(),
        other => {
            // Try metadata / request_header maps (Go `index .Map "key"` is the
            // idiomatic access; a bare `.SomeKey` on a map field doesn't resolve
            // in Go's text/template either — preserve that semantics).
            if let Some(value) = ctx.metadata.get(other) {
                return value.clone();
            }
            if let Some(value) = ctx.request_header.get(other) {
                return value.clone();
            }
            // Unknown field: Go's text/template would produce `<no value>`; we
            // keep the original token to make the miss obvious in tests.
            format!("{{{{{expr}}}}}")
        }
    }
}

/// Go `evaluateCondition`: render the condition template and return whether the
/// trimmed result equals `"true"`. Empty condition means always-execute.
pub fn evaluate_condition(condition: &str, ctx: &RenderContext) -> bool {
    if condition.is_empty() {
        return true;
    }
    render_template(condition, ctx).trim() == "true"
}

/// S19 — Apply a single [`OverrideOperation`] to a header map in place,
/// mirroring Go `applyOverrideOperationToHeaders`. The header map models
/// Go's `http.Header` (`BTreeMap<String, String>` is the Rust `HeaderMap`).
///
/// Supported ops: `set` (with `__CONDUIT_CLEAR__` sentinel), `delete`,
/// `rename`, `copy`. Array ops are no-ops on headers (Go's header switch
/// doesn't handle them; they fall through to the `default` case which only
/// logs).
pub fn apply_override_operation_to_headers(
    headers: &mut BTreeMap<String, String>,
    op: &OverrideOperation,
    ctx: &RenderContext,
) {
    if !evaluate_condition(&op.condition, ctx) {
        return;
    }

    match op.op.as_str() {
        override_op::SET => {
            let rendered = render_template(&op.value, ctx);
            if rendered == "__CONDUIT_CLEAR__" {
                headers.remove(&op.path);
                return;
            }
            headers.insert(op.path.clone(), rendered);
        }
        override_op::DELETE => {
            headers.remove(&op.path);
        }
        override_op::RENAME => {
            // Go's `http.Header.Values` returns all values for a key. Our map
            // is single-valued; mirror the multi-value path by treating one
            // entry like a single-element list.
            if let Some(value) = headers.remove(&op.from) {
                headers.insert(op.to.clone(), value);
            }
        }
        override_op::COPY => {
            // Go adds to `to` without removing `from`. Single-valued map: insert
            // overwrites any existing `to`; that matches Go's `headers.Add`
            // only when there was no prior `to`. `[Pascal ?]`: HeaderMap is
            // single-valued in the Rust port; multi-value header scenarios
            // (rare for overrides) may need revisiting.
            if let Some(value) = headers.get(&op.from).cloned() {
                headers.insert(op.to.clone(), value);
            }
        }
        _ => {
            // Unknown / array op: Go logs and does nothing for headers.
        }
    }
}

/// S19 — Apply a channel's header-override operations to an outbound raw
/// [`HttpRequest`] in place. Mirrors Go `applyOverrideRequestHeaders.OnRawRequest`.
///
/// `original_model` feeds [`build_render_context`] (Go `state.OriginalModel`),
/// used to resolve `{{.RequestModel}}`.
pub fn apply_override_request_headers(
    request: &mut HttpRequest,
    operations: &[OverrideOperation],
    llm_request: Option<&LlmRequest>,
    original_model: &str,
) {
    if operations.is_empty() {
        return;
    }

    let ctx = build_render_context(llm_request, original_model);
    for op in operations {
        apply_override_operation_to_headers(&mut request.headers, op, &ctx);
    }
}

// ---------------------------------------------------------------------------
// S20 — applyTransformOptions (Go: `transform_options.go`)
// ---------------------------------------------------------------------------

/// Decision output of [`apply_transform_options_decision`]. Mirrors what the Go
/// `applyTransformOptions(req, settings)` function would apply to the request.
///
/// The Go function returns a (possibly new) `*llm.Request` — either the same
/// pointer (no change) or a shallow clone with transform options applied. In
/// Rust we surface the decision as data so the caller can apply it; the
/// `changed` flag mirrors Go's same-pointer vs new-pointer distinction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransformOptionsDecision {
    /// `true` when `ChannelSettings.TransformOptions.force_array_instructions`
    /// is active — the request's `TransformOptions.ArrayInstructions` should be
    /// set to `Some(true)` (Go `lo.ToPtr(true)`).
    pub force_array_instructions: bool,
    /// `true` when `ChannelSettings.TransformOptions.force_array_inputs`
    /// is active — the request's `TransformOptions.ArrayInputs` should be
    /// set to `Some(true)`.
    pub force_array_inputs: bool,
    /// `true` when `ReplaceDeveloperRoleWithSystem` is active — the request's
    /// messages should have `developer` roles replaced with `system`.
    pub replace_developer_role_with_system: bool,
    /// `true` when any transform option is active (i.e., Go would create a new
    /// `*llm.Request` instead of returning the same pointer). Mirrors Go's
    /// `!transformOptions.ForceArrayInstructions && !...` early return.
    pub changed: bool,
}

/// S20 — Pure decision: which transform options should be applied given the
/// channel settings? Mirrors Go `applyTransformOptions`'s decision tree
/// (`transform_options.go` lines 14-26):
///
/// 1. nil settings → no change (Go returns same `req`).
/// 2. no flags active → no change (Go returns same `req`).
/// 3. any flag active → `changed = true` + per-flag booleans.
pub fn apply_transform_options_decision(
    channel_settings: Option<&ChannelSettings>,
) -> TransformOptionsDecision {
    let Some(settings) = channel_settings else {
        return TransformOptionsDecision::default();
    };
    let to = &settings.transform_options;
    if !to.force_array_instructions
        && !to.force_array_inputs
        && !to.replace_developer_role_with_system
    {
        return TransformOptionsDecision::default();
    }
    TransformOptionsDecision {
        force_array_instructions: to.force_array_instructions,
        force_array_inputs: to.force_array_inputs,
        replace_developer_role_with_system: to.replace_developer_role_with_system,
        changed: true,
    }
}

/// S20 — Replace `"developer"` role with `"system"` in messages
/// (case-insensitive). Mirrors Go `replaceDeveloperRoleWithSystem`
/// (`transform_options.go` lines 45-67).
///
/// Returns `true` when any role was replaced. The Go function allocates a new
/// slice only when a replacement happened (otherwise returns the original
/// slice); the `bool` return mirrors that "was anything replaced" signal so
/// the caller can decide whether to clone.
///
/// **Go parity detail:** Go uses `strings.EqualFold(msg.Role, "developer")`
/// which is Unicode case-folding — Rust's `eq_ignore_ascii_case` is an ASCII
/// subset. The Go test (`TestReplaceDeveloperRoleWithSystem`) only exercises
/// ASCII cases (`"Developer"`, `"DEVELOPER"`), so the ASCII predicate is
/// sufficient for parity. `[Faraday-the-26th ?]`: if non-ASCII developer roles
/// appear in the wild, switch to `unicase` or full Unicode case-folding.
pub fn replace_developer_role_with_system(messages: &mut [ChatMessage]) -> bool {
    let mut replaced = false;
    for msg in messages.iter_mut() {
        if msg.role.eq_ignore_ascii_case("developer") {
            msg.role = "system".to_string();
            replaced = true;
        }
    }
    replaced
}

/// S20 — Apply channel transform options to an [`LlmRequest`] in place.
/// Mirrors Go `applyTransformOptions(req, channelSettings) *llm.Request`
/// (`transform_options.go` lines 14-42).
///
/// Returns `true` when any transform option was active (i.e., Go would have
/// returned a new `*llm.Request` — `require.NotSame`). Returns `false` when
/// nil settings or no flags (Go returns the same pointer — `require.Same`).
///
/// **Parity caveat ([Faraday-the-26th ?]):** the Go function also sets
/// `newReq.TransformOptions.ArrayInstructions` / `ArrayInputs` on the request.
/// The Rust `ChatRequest` does not yet carry these fields (the request-level
/// `llm.TransformOptions` is not yet ported). The [`TransformOptionsDecision`]
/// surfaces these flags so the wiring layer can apply them once the fields
/// land. The developer-role replacement IS applied in-place here.
pub fn apply_transform_options(
    request: &mut LlmRequest,
    channel_settings: Option<&ChannelSettings>,
) -> bool {
    let decision = apply_transform_options_decision(channel_settings);
    if !decision.changed {
        return false;
    }

    if decision.replace_developer_role_with_system {
        let chat = chat_payload_or_default_mut(request);
        replace_developer_role_with_system(&mut chat.messages);
    }

    // force_array_instructions / force_array_inputs would set
    // request.TransformOptions.ArrayInstructions / ArrayInputs here.
    // The Rust ChatRequest lacks these fields; the decision struct surfaces
    // them for the wiring layer.
    decision.changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::SystemModelSettings;
    use conduit_core::objects::overrides::OverrideMatch;
    use conduit_llm::{ApiFormat, ChatMessage, ContentPart, RequestType};

    // ---------- Test constructors for non-Default types ----------

    /// Build a `ChatMessage` (which does not derive `Default`).
    fn chat_message(role: &str, content: Option<MessageContent>) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            name: None,
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    /// Build a text `ContentPart` (which does not derive `Default`).
    fn text_part(text: &str) -> ContentPart {
        ContentPart {
            part_type: "text".to_string(),
            text: Some(text.to_string()),
            image_url: None,
            input_audio: None,
            extra: BTreeMap::new(),
        }
    }

    /// Build a minimal `LlmRequest` (which does not derive `Default`) carrying
    /// an empty chat payload.
    fn llm_default() -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: None,
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: BTreeMap::new(),
            extra_headers: BTreeMap::new(),
            metadata: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    // ---------- S04 helpers ----------

    fn sys_message(text: &str) -> ChatMessage {
        chat_message("system", Some(MessageContent::Text(text.to_string())))
    }

    fn llm_with_sys_message(text: &str) -> LlmRequest {
        let mut req = llm_default();
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            messages: vec![sys_message(text)],
            ..Default::default()
        });
        req
    }

    /// Read the chat payload from `req` for assertions, panicking if it is not
    /// the chat variant (the helper is test-only).
    fn chat_ref_for_test(req: &LlmRequest) -> &ChatRequest {
        match &req.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => panic!("payload must stay chat"),
        }
    }

    // ---------- S04 tests ----------

    /// Mirrors Go `TestStripBillingHeaderCCH/strips cch from system string content and captures it`.
    #[test]
    fn s04_strip_billing_header_cch_from_text_content_and_captures() {
        let billing =
            "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli; cch=38a80;";
        let mut request = llm_with_sys_message(billing);

        let outcome = strip_billing_header_cch(&mut request);

        assert!(outcome.changed);
        assert_eq!(outcome.captured_cch, "38a80");

        let chat = chat_ref_for_test(&request);
        let content = match chat.messages[0].content.as_ref() {
            Some(MessageContent::Text(t)) => t.clone(),
            Some(other) => format!("unexpected: {other:?}"),
            None => String::new(),
        };
        assert!(content.starts_with("x-anthropic-billing-header:"));
        assert!(!content.contains("cch="));
        assert_eq!(
            content,
            "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli;"
        );
    }

    /// Mirrors Go `TestStripBillingHeaderCCH/strips cch from system multiple content part`.
    #[test]
    fn s04_strip_billing_header_cch_from_parts() {
        let billing =
            "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli; cch=abcde;";
        let mut request = llm_default();
        request.payload = LlmRequestPayload::Chat(ChatRequest {
            messages: vec![chat_message(
                "system",
                Some(MessageContent::Parts(vec![text_part(billing)])),
            )],
            ..Default::default()
        });

        let outcome = strip_billing_header_cch(&mut request);

        assert!(outcome.changed);
        assert_eq!(outcome.captured_cch, "abcde");

        let chat = chat_ref_for_test(&request);
        let rewritten = match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => match parts[0].text.as_deref() {
                Some(t) => t.to_string(),
                None => String::new(),
            },
            Some(other) => format!("unexpected: {other:?}"),
            None => String::new(),
        };
        assert!(rewritten.starts_with("x-anthropic-billing-header:"));
        assert!(!rewritten.contains("cch="));
    }

    /// Extra coverage: non-system messages and non-billing text are untouched.
    #[test]
    fn s04_leaves_non_billing_text_unchanged() {
        let mut request = llm_default();
        request.payload = LlmRequestPayload::Chat(ChatRequest {
            messages: vec![
                chat_message(
                    "user",
                    Some(MessageContent::Text(
                        "x-anthropic-billing-header: cch=xyz;".to_string(),
                    )),
                ),
                sys_message("just a regular system prompt"),
            ],
            ..Default::default()
        });

        let outcome = strip_billing_header_cch(&mut request);
        assert!(!outcome.changed);
        assert!(outcome.captured_cch.is_empty());
    }

    /// Extra: no `cch=` segment means no change (Go returns the original text).
    #[test]
    fn s04_billing_without_cch_is_unchanged() {
        let billing = "x-anthropic-billing-header: cc_version=2.1.42.c31;";
        let (new_text, cch, changed) = strip_billing_header_cch_from_text(billing);
        assert!(!changed);
        assert!(cch.is_empty());
        assert_eq!(new_text, billing);
    }

    #[test]
    fn s04_non_chat_payload_is_not_coerced_to_chat() {
        let mut request = llm_default();
        request.request_type = RequestType::Embedding;
        request.api_format = ApiFormat::OpenAiEmbeddings;
        request.payload = LlmRequestPayload::Embedding(Default::default());
        let original = request.payload.clone();

        let outcome = strip_billing_header_cch(&mut request);

        assert_eq!(outcome, BillingCchOutcome::default());
        assert_eq!(request.payload, original);
    }

    // ---------- S07 tests ----------

    /// Mirrors Go `TestSplitAutoReasoningEffortModel`.
    #[test]
    fn s07_split_auto_reasoning_effort_model_golden_table() {
        let cases: &[(&str, Option<(&str, &str)>)] = &[
            ("gpt-5.4-xhigh", Some(("gpt-5.4", "xhigh"))),
            ("gpt-5.4-max", Some(("gpt-5.4", "max"))),
            ("qwen3.7-max", None),
            ("qwen/qwen3-max", None),
            ("gpt-5.4-HIGH", Some(("gpt-5.4", "high"))),
            ("gpt-5.4-ultra", None),
            ("-high", None),
            ("gpt-5.4", None),
        ];

        for (input, expected) in cases {
            let got = split_auto_reasoning_effort_model(input);
            match expected {
                Some((base, effort)) => {
                    let (b, e) = match got {
                        Some(value) => value,
                        None => panic!("'{input}' should split"),
                    };
                    assert_eq!(b, *base, "base for {input}");
                    assert_eq!(e, *effort, "effort for {input}");
                }
                None => {
                    assert!(got.is_none(), "expected no split for {input}, got {got:?}");
                }
            }
        }
    }

    /// Mirrors Go `TestAutoReasoningEffortMiddleware_OnInboundLlmRequest`.
    #[test]
    fn s07_apply_auto_reasoning_effort_golden_table() {
        struct Case {
            name: &'static str,
            enabled: bool,
            model: &'static str,
            want_model: &'static str,
            want_effort: Option<&'static str>,
            pre_effort: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "disabled leaves request unchanged",
                enabled: false,
                model: "gpt-5.4-xhigh",
                want_model: "gpt-5.4-xhigh",
                want_effort: None,
                pre_effort: None,
            },
            Case {
                name: "enabled applies xhigh suffix",
                enabled: true,
                model: "gpt-5.4-xhigh",
                want_model: "gpt-5.4",
                want_effort: Some("xhigh"),
                pre_effort: None,
            },
            Case {
                name: "enabled applies max suffix",
                enabled: true,
                model: "gpt-5.4-max",
                want_model: "gpt-5.4",
                want_effort: Some("max"),
                pre_effort: None,
            },
            Case {
                name: "enabled keeps qwen max model unchanged",
                enabled: true,
                model: "qwen3.7-max",
                want_model: "qwen3.7-max",
                want_effort: None,
                pre_effort: None,
            },
            Case {
                name: "suffix overrides explicit request value",
                enabled: true,
                model: "gpt-5.4-xhigh",
                want_model: "gpt-5.4",
                want_effort: Some("xhigh"),
                pre_effort: Some("high"),
            },
            Case {
                name: "unsupported suffix is ignored",
                enabled: true,
                model: "gpt-5.4-ultra",
                want_model: "gpt-5.4-ultra",
                want_effort: None,
                pre_effort: None,
            },
        ];

        for case in cases {
            let mut request = llm_default();
            request.model = Some(case.model.to_string());
            request.payload = LlmRequestPayload::Chat(ChatRequest {
                reasoning_effort: case.pre_effort.map(str::to_string),
                ..Default::default()
            });
            let settings = SystemModelSettings {
                auto_reasoning_effort: case.enabled,
                ..Default::default()
            };

            apply_auto_reasoning_effort(&mut request, &settings);

            assert_eq!(
                request.model.as_deref(),
                Some(case.want_model),
                "{}",
                case.name
            );
            let chat = chat_ref_for_test(&request);
            assert_eq!(
                chat.reasoning_effort.as_deref(),
                case.want_effort,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn s07_is_qwen_max_model_matches_go() {
        assert!(is_qwen_max_model("qwen3.7-max"));
        assert!(is_qwen_max_model("qwen/qwen3-max"));
        assert!(is_qwen_max_model("Qwen-Max"));
        assert!(!is_qwen_max_model("gpt-5.4-max"));
        assert!(!is_qwen_max_model("qwen-max-something"));
    }

    // ---------- S08 helpers ----------

    /// Synthetic API-key view for tests.
    struct TestApiKey {
        name: String,
        active: String,
        model_ids: Vec<String>,
        mappings: Vec<ModelMapping>,
    }

    impl ApiKeyProfileView for TestApiKey {
        fn name(&self) -> &str {
            &self.name
        }
        fn active_profile_name(&self) -> &str {
            &self.active
        }
        fn model_ids(&self) -> &[String] {
            &self.model_ids
        }
        fn model_mappings(&self) -> &[ModelMapping] {
            &self.mappings
        }
    }

    /// Mirrors Go `model_access_test.go` "empty model errors".
    #[test]
    fn s08_empty_model_errors_with_invalid_model() {
        let result = check_api_key_model_access("", None);
        let err = match result {
            Err(e) => e,
            Ok(()) => panic!("empty model should error"),
        };
        assert_eq!(err.message, "Invalid model: request model is empty");
        assert_eq!(err.error_type(), "invalid_model");
    }

    #[test]
    fn s08_no_api_key_allows_any_model() {
        let result = check_api_key_model_access("gpt-4", None);
        assert!(result.is_ok(), "no api key should allow: {result:?}");
    }

    #[test]
    fn s08_empty_allow_list_allows_any_model() {
        let key = TestApiKey {
            name: "k".to_string(),
            active: "p".to_string(),
            model_ids: vec![],
            mappings: vec![],
        };
        let result = check_api_key_model_access("anything", Some(&key));
        assert!(result.is_ok(), "empty allow list should allow: {result:?}");
    }

    #[test]
    fn s08_allowed_model_passes() {
        let key = TestApiKey {
            name: "k".to_string(),
            active: "p".to_string(),
            model_ids: vec!["gpt-4".to_string(), "claude-3".to_string()],
            mappings: vec![],
        };
        let result = check_api_key_model_access("gpt-4", Some(&key));
        assert!(result.is_ok(), "in allow list should pass: {result:?}");
    }

    #[test]
    fn s08_denied_model_returns_invalid_model() {
        let key = TestApiKey {
            name: "k".to_string(),
            active: "p".to_string(),
            model_ids: vec!["gpt-4".to_string()],
            mappings: vec![],
        };
        let err = match check_api_key_model_access("claude-3", Some(&key)) {
            Err(e) => e,
            Ok(()) => panic!("denied model should error"),
        };
        assert_eq!(err.error_type(), "invalid_model");
        assert!(err.message.ends_with("claude-3"));
    }

    // ---------- S09 tests ----------

    #[test]
    fn s09_exact_match_mapping() {
        let mappings = vec![ModelMapping {
            from: "gpt-4".to_string(),
            to: "gpt-4-turbo".to_string(),
        }];
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "gpt-4"),
            "gpt-4-turbo"
        );
    }

    #[test]
    fn s09_no_match_returns_original() {
        let mappings = vec![ModelMapping {
            from: "gpt-4".to_string(),
            to: "gpt-4-turbo".to_string(),
        }];
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "claude-3"),
            "claude-3"
        );
    }

    #[test]
    fn s09_wildcard_pattern_matches_anything() {
        let mappings = vec![ModelMapping {
            from: "*".to_string(),
            to: "renamed".to_string(),
        }];
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "anything"),
            "renamed"
        );
    }

    #[test]
    fn s09_regex_pattern_anchored_full_match() {
        // Go anchors the pattern; `gpt-.*` matches `gpt-4` and `gpt-5-turbo`.
        let mappings = vec![ModelMapping {
            from: "gpt-.*".to_string(),
            to: "gpt-family".to_string(),
        }];
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "gpt-4"),
            "gpt-family"
        );
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "gpt-5-turbo"),
            "gpt-family"
        );
        // Anchored: must match the whole string.
        assert_eq!(
            ModelMapper::map_model_owned(&mappings, "prefix-gpt-4"),
            "prefix-gpt-4"
        );
    }

    #[test]
    fn s09_first_matching_entry_wins() {
        let mappings = vec![
            ModelMapping {
                from: "gpt-4".to_string(),
                to: "winner".to_string(),
            },
            ModelMapping {
                from: "*".to_string(),
                to: "loser".to_string(),
            },
        ];
        assert_eq!(ModelMapper::map_model_owned(&mappings, "gpt-4"), "winner");
        assert_eq!(ModelMapper::map_model_owned(&mappings, "other"), "loser");
    }

    #[test]
    fn s09_invalid_regex_returns_false_silently() {
        // Unbalanced bracket => compile error => false.
        assert!(!ModelMapper::matches_mapping("[invalid", "anything"));
    }

    #[test]
    fn s09_apply_model_mapping_rewrites_request_model() {
        let key = TestApiKey {
            name: "k".to_string(),
            active: "p".to_string(),
            model_ids: vec![],
            mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "gpt-4-turbo".to_string(),
            }],
        };
        let mut request = llm_default();
        request.model = Some("gpt-4".to_string());

        let original = match apply_model_mapping(&mut request, Some(&key)) {
            Ok(value) => value,
            Err(e) => panic!("mapping should succeed: {e}"),
        };
        assert_eq!(original, "gpt-4");
        assert_eq!(request.model.as_deref(), Some("gpt-4-turbo"));
    }

    #[test]
    fn s09_apply_model_mapping_empty_model_errors() {
        let mut request = llm_default();
        request.model = Some(String::new());
        let err = match apply_model_mapping(&mut request, None) {
            Err(e) => e,
            Ok(_) => panic!("empty model should error"),
        };
        assert!(err.message.ends_with("request model is empty"));
    }

    // ------- model_mapper_test.go golden tables (Faraday-28th) -------

    /// Mirrors Go `TestModelMapper_MatchesMapping` (model_mapper_test.go L162-221).
    /// Each row is a direct port of the Go table case, citing the Go test name.
    #[test]
    fn s09_matches_mapping_golden_table() -> Result<(), Box<dyn std::error::Error>> {
        let cases: &[(&str, &str, &str, bool)] = &[
            // (Go test name, pattern, str, expected)
            ("exact match", "gpt-4", "gpt-4", true),
            ("no match", "gpt-*", "claude-3", false),
            ("wildcard only", "*", "any-model", true),
            ("regex special chars escaped", "model.v1", "model.v1", true),
            // Go name says "no match" but expected=true: the `.` metachar in
            // regex mode matches any char, so "modelxv1" matches "model.v1".
            ("regex special chars no match", "model.v1", "modelxv1", true),
            ("invalid regex returns false", "[invalid", "[invalid", false),
            (
                "invalid regex returns false for any string",
                "[invalid",
                "other",
                false,
            ),
        ];

        for (name, pattern, input, expected) in cases {
            let got = ModelMapper::matches_mapping(pattern, input);
            assert_eq!(
                got, *expected,
                "matches_mapping({pattern:?}, {input:?}) -- Go case: {name}"
            );
        }
        Ok(())
    }

    /// Mirrors the mapping-level rows of Go `TestModelMapper_MapModel`
    /// (model_mapper_test.go L57-132) — the cases where an active profile
    /// exists and mappings are applied. Each row cites the Go test name.
    ///
    /// The profile-resolution rows (nil key L23-28, no profiles L29-37, no
    /// active profile L38-56, profile-not-found L133-151) are covered by
    /// `s09_apply_model_mapping_nil_api_key_noop` and
    /// `s09_apply_model_mapping_no_active_mappings_noop` below — in Rust those
    /// branches collapse into "api_key is None" and "mappings list is empty".
    #[test]
    fn s09_map_model_owned_golden_table() -> Result<(), Box<dyn std::error::Error>> {
        // (Go test name, from, to, original, expected)
        let cases: &[(&str, &str, &str, &str, &str)] = &[
            (
                "active profile with exact match",
                "gpt-4",
                "claude-3-opus",
                "gpt-4",
                "claude-3-opus",
            ),
            (
                "active profile with regexp match",
                "gpt-.*",
                "claude-3-opus",
                "gpt-4-turbo",
                "claude-3-opus",
            ),
            (
                "active profile with regexp match 2",
                "claude.*-haiku.*",
                "deepseek-chat",
                "claude-haiku-4-5-20251001",
                "deepseek-chat",
            ),
            (
                "active profile with no matching mapping",
                "gpt-4",
                "claude-3-opus",
                "gpt-3.5-turbo",
                "gpt-3.5-turbo",
            ),
        ];

        for (name, from, to, original, expected) in cases {
            let mappings = vec![ModelMapping {
                from: from.to_string(),
                to: to.to_string(),
            }];
            let got = ModelMapper::map_model_owned(&mappings, original);
            assert_eq!(
                got, *expected,
                "map_model_owned -- Go case: {name} (original={original})"
            );
        }
        Ok(())
    }

    /// Mirrors Go `TestModelMapper_MapModel` case "nil api key"
    /// (model_mapper_test.go L23-28): when the API key is absent the model is
    /// returned unchanged. In Rust this is `apply_model_mapping(.., None)`.
    #[test]
    fn s09_apply_model_mapping_nil_api_key_noop() -> Result<(), Box<dyn std::error::Error>> {
        let mut request = llm_default();
        request.model = Some("gpt-4".to_string());
        let original = apply_model_mapping(&mut request, None)?;
        assert_eq!(original, "gpt-4");
        assert_eq!(request.model.as_deref(), Some("gpt-4"));
        Ok(())
    }

    /// Mirrors Go `TestModelMapper_MapModel` cases "no profiles" (L29-37),
    /// "no active profile" (L38-56), and "active profile not found in profiles
    /// list" (L133-151). In the Rust `ApiKeyProfileView` abstraction the profile
    /// resolution is delegated to the implementor: all three Go scenarios
    /// collapse to "the key has zero model mappings to apply". The model must be
    /// returned unchanged.
    #[test]
    fn s09_apply_model_mapping_no_active_mappings_noop() -> Result<(), Box<dyn std::error::Error>> {
        // Empty mappings (covers Go "no profiles", "no active profile",
        // and "active profile not found in profiles list").
        let key = TestApiKey {
            name: "test-key".to_string(),
            active: String::new(), // no active profile
            model_ids: vec![],
            mappings: vec![],
        };
        let mut request = llm_default();
        request.model = Some("gpt-4".to_string());
        let original = apply_model_mapping(&mut request, Some(&key))?;
        assert_eq!(original, "gpt-4");
        assert_eq!(request.model.as_deref(), Some("gpt-4"));
        Ok(())
    }

    // ---------- S18 tests ----------

    #[test]
    fn s18_disabled_uses_default_user_agent() {
        assert_eq!(
            decide_user_agent(Some(false), true, Some("client-ua")),
            DEFAULT_USER_AGENT
        );
        assert_eq!(
            decide_user_agent(None, false, Some("client-ua")),
            DEFAULT_USER_AGENT
        );
    }

    #[test]
    fn s18_enabled_with_client_ua_passes_through() {
        assert_eq!(
            decide_user_agent(Some(true), false, Some("client-ua")),
            "client-ua"
        );
    }

    #[test]
    fn s18_enabled_without_client_ua_falls_back_to_default() {
        assert_eq!(
            decide_user_agent(Some(true), false, None),
            DEFAULT_USER_AGENT
        );
        assert_eq!(
            decide_user_agent(Some(true), false, Some("")),
            DEFAULT_USER_AGENT
        );
    }

    #[test]
    fn s18_channel_override_takes_precedence_over_global() {
        // Channel false overrides a global true.
        assert_eq!(
            decide_user_agent(Some(false), true, Some("client-ua")),
            DEFAULT_USER_AGENT
        );
        // Channel true overrides a global false.
        assert_eq!(
            decide_user_agent(Some(true), false, Some("client-ua")),
            "client-ua"
        );
    }

    #[test]
    fn s18_apply_writes_header_into_request() {
        let mut request = HttpRequest::default();
        let written = apply_user_agent_pass_through(&mut request, Some(true), false, Some("ua-x"));
        assert_eq!(written, "ua-x");
        assert_eq!(
            request.headers.get("User-Agent").map(String::as_str),
            Some("ua-x")
        );
    }

    // ---------- S19 helpers ----------

    fn op_set(path: &str, value: &str) -> OverrideOperation {
        OverrideOperation {
            op: override_op::SET.to_string(),
            path: path.to_string(),
            value: value.to_string(),
            ..Default::default()
        }
    }

    fn op_delete(path: &str) -> OverrideOperation {
        OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: path.to_string(),
            ..Default::default()
        }
    }

    fn op_rename(from: &str, to: &str) -> OverrideOperation {
        OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            ..Default::default()
        }
    }

    fn op_copy(from: &str, to: &str) -> OverrideOperation {
        OverrideOperation {
            op: override_op::COPY.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            ..Default::default()
        }
    }

    // ---------- S19 tests ----------

    /// Mirrors Go `TestOverrideParametersWithTemplate` header branch
    /// (`X-Custom-Model: header-{{.Model}}`).
    #[test]
    fn s19_set_renders_template_against_model() {
        let mut headers = BTreeMap::new();
        let op = op_set("X-Custom-Model", "header-{{.Model}}");
        let ctx = RenderContext {
            model: "gpt-4".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op, &ctx);
        assert_eq!(
            headers.get("X-Custom-Model").map(String::as_str),
            Some("header-gpt-4")
        );
    }

    #[test]
    fn s19_set_clear_sentinel_deletes_header() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Old".to_string(), "v".to_string());
        let op = op_set("X-Old", "__CONDUIT_CLEAR__");
        apply_override_operation_to_headers(&mut headers, &op, &RenderContext::default());
        assert!(!headers.contains_key("X-Old"));
    }

    #[test]
    fn s19_delete_removes_header() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Remove-Me".to_string(), "v".to_string());
        apply_override_operation_to_headers(
            &mut headers,
            &op_delete("X-Remove-Me"),
            &RenderContext::default(),
        );
        assert!(!headers.contains_key("X-Remove-Me"));
    }

    #[test]
    fn s19_rename_moves_value() {
        let mut headers = BTreeMap::new();
        headers.insert("From".to_string(), "v".to_string());
        apply_override_operation_to_headers(
            &mut headers,
            &op_rename("From", "To"),
            &RenderContext::default(),
        );
        assert!(!headers.contains_key("From"));
        assert_eq!(headers.get("To").map(String::as_str), Some("v"));
    }

    #[test]
    fn s19_rename_missing_key_is_noop() {
        let mut headers = BTreeMap::new();
        apply_override_operation_to_headers(
            &mut headers,
            &op_rename("From", "To"),
            &RenderContext::default(),
        );
        assert!(headers.is_empty());
    }

    #[test]
    fn s19_copy_keeps_source_and_adds_destination() {
        let mut headers = BTreeMap::new();
        headers.insert("From".to_string(), "v".to_string());
        apply_override_operation_to_headers(
            &mut headers,
            &op_copy("From", "To"),
            &RenderContext::default(),
        );
        assert_eq!(headers.get("From").map(String::as_str), Some("v"));
        assert_eq!(headers.get("To").map(String::as_str), Some("v"));
    }

    #[test]
    fn s19_condition_false_skips_op() {
        let mut headers = BTreeMap::new();
        let mut op = op_set("X-Cond", "v");
        op.condition = "{{.Model}}".to_string(); // renders to "gpt-4" != "true"
        let ctx = RenderContext {
            model: "gpt-4".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op, &ctx);
        assert!(headers.is_empty());
    }

    #[test]
    fn s19_condition_true_executes_op() {
        let mut headers = BTreeMap::new();
        let mut op = op_set("X-Cond", "v");
        op.condition = "{{.IsEnabled}}".to_string();
        let mut ctx = RenderContext::default();
        ctx.metadata
            .insert("IsEnabled".to_string(), "true".to_string());
        apply_override_operation_to_headers(&mut headers, &op, &ctx);
        assert_eq!(headers.get("X-Cond").map(String::as_str), Some("v"));
    }

    #[test]
    fn s19_apply_override_request_headers_no_ops_is_noop() {
        let mut request = HttpRequest::default();
        request.headers.insert("Keep".to_string(), "v".to_string());
        apply_override_request_headers(&mut request, &[], None, "");
        assert_eq!(request.headers.get("Keep").map(String::as_str), Some("v"));
    }

    #[test]
    fn s19_apply_override_request_headers_renders_full_context() {
        let mut request = HttpRequest::default();
        let ops = vec![
            op_set("X-Model", "m-{{.Model}}"),
            op_set("X-Req-Model", "rm-{{.RequestModel}}"),
        ];

        let mut llm = llm_default();
        llm.model = Some("gpt-4o".to_string());

        apply_override_request_headers(&mut request, &ops, Some(&llm), "client-model");
        assert_eq!(
            request.headers.get("X-Model").map(String::as_str),
            Some("m-gpt-4o")
        );
        assert_eq!(
            request.headers.get("X-Req-Model").map(String::as_str),
            Some("rm-client-model")
        );
    }

    #[test]
    fn s19_build_render_context_filters_sensitive_headers() {
        // `LlmRequest.extra_headers` is the Rust shape closest to Go's
        // `RawRequest.Headers`. It is single-valued (`HeaderMap = BTreeMap<String,
        // String>`), so the multi-value branch is exercised separately by
        // `build_request_header_map`'s own JSON-snapshot tests.
        let mut llm = llm_default();
        llm.extra_headers
            .insert("X-Trace-Id".to_string(), "trace-123".to_string());
        llm.extra_headers
            .insert("Authorization".to_string(), "Bearer secret".to_string());
        llm.extra_headers
            .insert("X-API-Key".to_string(), "k".to_string());

        let ctx = build_render_context(Some(&llm), "orig");
        assert_eq!(
            ctx.request_header.get("X-Trace-Id").map(String::as_str),
            Some("trace-123")
        );
        assert_eq!(
            ctx.request_header.get("x-trace-id").map(String::as_str),
            Some("trace-123")
        );
        assert!(!ctx.request_header.contains_key("Authorization"));
        assert!(!ctx.request_header.contains_key("authorization"));
        assert!(!ctx.request_header.contains_key("X-API-Key"));
        assert_eq!(ctx.request_model, "orig");
    }

    /// Multi-value header snapshot path: `build_request_header_map` accepts a
    /// JSON-typed raw-request snapshot (Go shape) and takes the first value of
    /// array-typed headers.
    #[test]
    fn s19_build_request_header_map_takes_first_of_multi_value() {
        let raw = serde_json::json!({
            "headers": {
                "X-Multi-Value": ["first", "second"],
                "X-Single": "only",
            }
        });
        let map = build_request_header_map(Some(&raw));
        assert_eq!(map.get("X-Multi-Value").map(String::as_str), Some("first"));
        assert_eq!(map.get("X-Single").map(String::as_str), Some("only"));
    }

    // ---------- Render-template sanity ----------

    #[test]
    fn render_template_no_braces_returns_input() {
        assert_eq!(render_template("plain", &RenderContext::default()), "plain");
    }

    #[test]
    fn render_template_unknown_field_returns_token_unchanged() {
        assert_eq!(
            render_template("v-{{.Missing}}", &RenderContext::default()),
            "v-{{.Missing}}"
        );
    }

    #[test]
    fn canonical_header_key_matches_go_for_common_cases() {
        assert_eq!(canonical_header_key("x-trace-id"), "X-Trace-Id");
        assert_eq!(canonical_header_key("user-agent"), "User-Agent");
        assert_eq!(canonical_header_key("X-API-Key"), "X-Api-Key");
    }

    // Override unused-import guard for `OverrideMatch` (referenced via re-export
    // only in the docs).
    #[test]
    fn override_match_type_is_reachable() {
        let _ = OverrideMatch::default();
    }

    // ---------- S20 — transform_options tests ----------
    //   (mirror Go `transform_options_test.go`, 141 lines)
    //
    // Go source: `conduit/internal/server/orchestrator/transform_options.go`.
    // The Go `applyTransformOptions` returns a (possibly new) `*llm.Request`;
    // pointer identity (`require.Same` / `require.NotSame`) distinguishes the
    // no-change path from the clone path. In Rust the `apply_transform_options`
    // helper returns `bool` (`changed`) which mirrors that distinction.

    use conduit_core::objects::channel_settings::{
        ChannelSettings as CoreChannelSettings, TransformOptions as CoreTransformOptions,
    };

    fn settings_with(opts: CoreTransformOptions) -> CoreChannelSettings {
        CoreChannelSettings {
            transform_options: opts,
            ..Default::default()
        }
    }

    fn chat_with_roles(roles: &[&str]) -> LlmRequest {
        let messages: Vec<ChatMessage> = roles
            .iter()
            .map(|r| chat_message(r, Some(MessageContent::Text(format!("content-{r}")))))
            .collect();
        let mut req = llm_default();
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            messages,
            ..Default::default()
        });
        req
    }

    fn message_roles(req: &LlmRequest) -> Vec<String> {
        match &req.payload {
            LlmRequestPayload::Chat(c) => c.messages.iter().map(|m| m.role.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// Mirrors Go `TestApplyTransformOptions_ReplaceDeveloperRoleWithSystem`
    /// (transform_options_test.go lines 13-35): with
    /// `ReplaceDeveloperRoleWithSystem = true`, developer→system replacement
    /// happens and a new request is produced (changed = true).
    #[test]
    fn s20_replace_developer_role_with_system_enabled() {
        let mut req = chat_with_roles(&["developer", "user"]);
        let settings = settings_with(CoreTransformOptions {
            replace_developer_role_with_system: true,
            ..Default::default()
        });

        let changed = apply_transform_options(&mut req, Some(&settings));

        assert!(changed, "changed must be true when flag is active");
        let roles = message_roles(&req);
        assert_eq!(roles[0], "system", "developer → system");
        assert_eq!(roles[1], "user", "user role unchanged");
    }

    /// Mirrors Go `TestApplyTransformOptions_KeepDeveloperRoleWhenDisabled`
    /// (transform_options_test.go lines 37-56): with the flag `false`, the
    /// developer role is kept and the same request is returned (changed = false).
    #[test]
    fn s20_keep_developer_role_when_disabled() {
        let mut req = chat_with_roles(&["developer"]);
        let settings = settings_with(CoreTransformOptions {
            replace_developer_role_with_system: false,
            ..Default::default()
        });

        let changed = apply_transform_options(&mut req, Some(&settings));

        assert!(!changed, "changed must be false when no flags active");
        assert_eq!(message_roles(&req)[0], "developer");
    }

    /// Mirrors Go `TestApplyTransformOptions_NilSettings`
    /// (transform_options_test.go lines 58-64): nil settings returns the same
    /// request (changed = false), no mutation.
    #[test]
    fn s20_nil_settings_no_change() {
        let mut req = chat_with_roles(&["developer"]);

        let changed = apply_transform_options(&mut req, None);

        assert!(!changed, "nil settings must not change the request");
        assert_eq!(message_roles(&req)[0], "developer");
    }

    /// Mirrors Go `TestApplyTransformOptions_ForceArrayInstructions`
    /// (transform_options_test.go lines 66-79): with
    /// `ForceArrayInstructions = true`, the decision surfaces
    /// `force_array_instructions = true` and changed = true.
    ///
    /// The Go test asserts `result.TransformOptions.ArrayInstructions ==
    /// lo.ToPtr(true)`. The Rust `ChatRequest` does not yet carry these fields,
    /// so the decision struct is the contract surface.
    #[test]
    fn s20_force_array_instructions_decision() {
        let settings = settings_with(CoreTransformOptions {
            force_array_instructions: true,
            ..Default::default()
        });

        let decision = apply_transform_options_decision(Some(&settings));

        assert!(decision.changed);
        assert!(decision.force_array_instructions);
        assert!(!decision.force_array_inputs);
        assert!(!decision.replace_developer_role_with_system);
    }

    /// Mirrors Go `TestApplyTransformOptions_ForceArrayInputs`
    /// (transform_options_test.go lines 81-94): with `ForceArrayInputs = true`,
    /// the decision surfaces `force_array_inputs = true` and changed = true.
    #[test]
    fn s20_force_array_inputs_decision() {
        let settings = settings_with(CoreTransformOptions {
            force_array_inputs: true,
            ..Default::default()
        });

        let decision = apply_transform_options_decision(Some(&settings));

        assert!(decision.changed);
        assert!(!decision.force_array_instructions);
        assert!(decision.force_array_inputs);
        assert!(!decision.replace_developer_role_with_system);
    }

    /// Mirrors Go `TestReplaceDeveloperRoleWithSystem`
    /// (transform_options_test.go lines 96-141): table-driven test with 4
    /// sub-cases — empty messages, developer→system, case-insensitive, and
    /// no-developer.
    #[test]
    fn s20_replace_developer_role_with_system_golden_table() {
        // Sub-case: "empty messages" — no messages, nothing replaced.
        {
            let mut msgs: Vec<ChatMessage> = Vec::new();
            let replaced = replace_developer_role_with_system(&mut msgs);
            assert!(!replaced, "empty messages: nothing replaced");
            assert!(msgs.is_empty());
        }

        // Sub-case: "developer role replaced" — developer + user → system + user.
        {
            let mut msgs = vec![chat_message("developer", None), chat_message("user", None)];
            let replaced = replace_developer_role_with_system(&mut msgs);
            assert!(replaced, "developer role: replaced");
            assert_eq!(msgs[0].role, "system");
            assert_eq!(msgs[1].role, "user");
        }

        // Sub-case: "Developer case insensitive" — Developer + DEVELOPER → system + system.
        {
            let mut msgs = vec![
                chat_message("Developer", None),
                chat_message("DEVELOPER", None),
            ];
            let replaced = replace_developer_role_with_system(&mut msgs);
            assert!(replaced, "case-insensitive: replaced");
            assert_eq!(msgs[0].role, "system");
            assert_eq!(msgs[1].role, "system");
        }

        // Sub-case: "no developer role" — system + user unchanged.
        {
            let mut msgs = vec![chat_message("system", None), chat_message("user", None)];
            let replaced = replace_developer_role_with_system(&mut msgs);
            assert!(!replaced, "no developer role: nothing replaced");
            assert_eq!(msgs[0].role, "system");
            assert_eq!(msgs[1].role, "user");
        }
    }
}

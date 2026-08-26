//! RUST-P9-006 - orchestrator pass-through pure-logic helpers (batch 2).
//!
//! Each function in this module mirrors one inbound/outbound middleware from
//! `conduit/internal/server/orchestrator/pass_through.go` /
//! `conduit/internal/server/orchestrator/override.go` (the
//! `ChatCompletionOrchestrator.Process` middleware chain). The Go middlewares
//! close over runtime collaborators (`*biz.SystemService`,
//! `*PersistentInboundTransformer`, `*PersistentOutboundTransformer`); here we
//! extract the *pure decision logic* so it is unit-testable without those
//! heavy types (which are not yet ported). The orchestrator wiring will call
//! these helpers when RUST-P9-006 S29 lands.
//!
//! Scope of this file (matches TODO_SMALL `[RUST-P9-006]` entries):
//! - **S13** [`apply_pass_through_response`] /
//!   [`PassThroughResponsePlan`] - Go `applyPassThroughResponse`
//!   (`internal/server/orchestrator/pass_through.go:233-255`).
//! - **S14** [`apply_pass_through_stream`] / [`PassThroughStreamPlan`] - Go
//!   `applyPassThroughStream` (`pass_through.go:346-383`).
//! - **S16** [`apply_pass_through_request_body`] /
//!   [`PassThroughRequestBodyPlan`] - Go `applyPassThroughRequestBody` +
//!   `mergePassThroughRequestBody` + `passThroughBodySupported` +
//!   `passThroughBodyNeedsModelPatch` (`pass_through.go:65-160`).
//! - **S17** [`apply_override_request_body`] /
//!   [`OverrideRequestBodyPlan`] - Go `applyOverrideRequestBody` +
//!   `applyBodyOperation` (`override.go:111-208`).
//!
//! Shared predicate:
//! - [`is_pass_through_enabled`] - Go `isPassThroughEnabled` +
//!   `passThroughStreamAligned` (`pass_through.go:18-62`).
//!
//! Go-parity doubts and blockers are flagged inline with `[Faraday ?]`.

#![forbid(unsafe_code)]

use conduit_core::objects::overrides::{OverrideOperation, override_op};
use conduit_llm::ApiFormat;
use serde_json::Value;

// Re-use the template / render-context helpers already ported for S18/S19 so
// the body-override semantics stay consistent with the header-override port.
pub use crate::pre_execution::{
    RenderContext, build_render_context, evaluate_condition, render_template,
};

// ===========================================================================
// Shared predicate - isPassThroughEnabled (Go `pass_through.go:18-62`)
// ===========================================================================

/// Inputs to [`is_pass_through_enabled`]. The wiring layer resolves every
/// branch of Go's `isPassThroughEnabled` against its `*PersistenceState` and
/// passes the pre-computed values here. This mirrors the
/// "collapse-each-branch-into-a-bool" convention already used by
/// [`crate::orchestrator::capture_plan`] (S27/S28).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassThroughInputs {
    /// `true` when a current channel is selected (Go `channel != nil`).
    pub has_channel: bool,
    /// `true` when `state.RawProviderRequest != nil && api_format != ""`.
    pub has_raw_provider_request: bool,
    /// `true` when `state.LlmRequest != nil &&
    /// llmReq.APIFormat == rawProviderRequest.APIFormat`.
    pub api_formats_match: bool,
    /// `true` when `passThroughStreamAligned(original, llmReq.Stream)`.
    pub streams_aligned: bool,
    /// Pre-resolved effective flag: channel-level `PassThroughBody` when set,
    /// otherwise the global `SystemService.PassThrough(ctx)` result.
    /// `None`/`false` means "global lookup failed / not consulted" - Go treats
    /// that as `false`.
    pub pass_through_body_enabled: bool,
}

/// S13/S14/S16/S27/S28 - Pure port of Go
/// `passThroughStreamAligned(originalStream, effectiveStream)`
/// (`internal/server/orchestrator/pass_through.go:64-69`).
///
/// The Rust `LlmRequest.stream` field is a plain `bool` (no `Option`), so the
/// effective side is a `bool`. The original side is `Option<bool>` to mirror
/// Go's `state.OriginalRequestStream *bool` (Go distinguishes "absent" from
/// "explicit false").
pub fn pass_through_stream_aligned(original: Option<bool>, effective: bool) -> bool {
    let original_enabled = original.unwrap_or(false);
    original_enabled == effective
}

/// S13/S14/S16/S27/S28 - Pure port of Go `isPassThroughEnabled`
/// (`internal/server/orchestrator/pass_through.go:18-62`).
///
/// Returns `true` iff every gate passes (channel selected, raw provider request
/// present with non-empty api format, inbound/outbound API formats identical,
/// stream flags aligned, and the effective pass-through-body flag enabled).
///
/// `[Faraday ?]`: collapses Go's multi-branch function into a single boolean
/// conjunction. Faithful because every branch's terminal action is
/// `return false`. The wiring layer is responsible for evaluating each branch
/// against its own state and passing the results via [`PassThroughInputs`].
pub fn is_pass_through_enabled(inputs: &PassThroughInputs) -> bool {
    inputs.has_channel
        && inputs.has_raw_provider_request
        && inputs.api_formats_match
        && inputs.streams_aligned
        && inputs.pass_through_body_enabled
}

// ===========================================================================
// S13 - applyPassThroughResponse (Go `pass_through.go:233-255`)
// ===========================================================================

/// Outcome of [`apply_pass_through_response`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassThroughResponsePlan {
    /// Pass-through disabled or no raw response captured - emit the
    /// transformed response unchanged (Go `return response, nil`).
    Transformed,
    /// Pass-through enabled AND a raw provider response is available - emit
    /// the **raw** provider response (Go `return rawResp, nil`).
    Raw,
}

impl PassThroughResponsePlan {
    /// `true` when this plan is [`PassThroughResponsePlan::Raw`].
    pub const fn is_raw(self) -> bool {
        matches!(self, Self::Raw)
    }

    /// `true` when this plan is [`PassThroughResponsePlan::Transformed`].
    pub const fn is_transformed(self) -> bool {
        matches!(self, Self::Transformed)
    }
}

/// S13 - Pure decision mirroring Go `applyPassThroughResponse.OnInboundRawResponse`
/// (`internal/server/orchestrator/pass_through.go:233-255`).
///
/// Inputs:
/// - `pass_through_enabled` - pre-resolved [`is_pass_through_enabled`] result.
/// - `has_raw_response` - `true` when `state.RawProviderResponse != nil`.
///
/// Decision:
/// - `pass_through_enabled && has_raw_response` -> [`PassThroughResponsePlan::Raw`].
/// - otherwise -> [`PassThroughResponsePlan::Transformed`].
pub fn apply_pass_through_response(
    pass_through_enabled: bool,
    has_raw_response: bool,
) -> PassThroughResponsePlan {
    if pass_through_enabled && has_raw_response {
        PassThroughResponsePlan::Raw
    } else {
        PassThroughResponsePlan::Transformed
    }
}

// ===========================================================================
// S14 - applyPassThroughStream (Go `pass_through.go:346-383`)
// ===========================================================================

/// Outcome of [`apply_pass_through_stream`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassThroughStreamPlan {
    /// Pass-through disabled, or no raw stream channel was captured - emit the
    /// transformed stream unchanged (Go `return stream, nil`).
    Transformed,
    /// Pass-through enabled AND a raw stream channel exists - emit the **raw**
    /// provider stream events (Go `return &passThroughChannelStream{ch:
    /// rawCh,...}, nil`).
    Raw,
}

impl PassThroughStreamPlan {
    /// `true` when this plan is [`PassThroughStreamPlan::Raw`].
    pub const fn is_raw(self) -> bool {
        matches!(self, Self::Raw)
    }

    /// `true` when this plan is [`PassThroughStreamPlan::Transformed`].
    pub const fn is_transformed(self) -> bool {
        matches!(self, Self::Transformed)
    }
}

/// S14 - Pure decision mirroring Go `applyPassThroughStream.OnInboundRawStream`
/// (`internal/server/orchestrator/pass_through.go:346-383`).
///
/// `[Faraday ?]`: the goroutine that drains the transformed pipeline stream
/// (so LLM middlewares like connection tracking / performance recording still
/// observe events) is I/O owned by the wiring layer; the pure plan only
/// answers "which stream does the client see?".
pub fn apply_pass_through_stream(
    pass_through_enabled: bool,
    has_raw_stream: bool,
) -> PassThroughStreamPlan {
    if pass_through_enabled && has_raw_stream {
        PassThroughStreamPlan::Raw
    } else {
        PassThroughStreamPlan::Transformed
    }
}

// ===========================================================================
// S16 - applyPassThroughRequestBody (Go `pass_through.go:65-160`)
// ===========================================================================

/// `true` for API formats whose raw inbound body can safely replace the
/// outbound request body. Mirrors Go `passThroughBodySupported`
/// (`pass_through.go:142-156`). Multipart formats are excluded because the
/// outbound transformer rebuilds the multipart payload with a fresh boundary.
pub fn pass_through_body_supported(api_format: ApiFormat) -> bool {
    !matches!(
        api_format,
        ApiFormat::OpenAiAudioTranscriptions
            | ApiFormat::OpenAiAudioTranslations
            | ApiFormat::OpenAiImageEdit
            | ApiFormat::OpenAiImageVariation
    )
}

/// `true` for API formats whose request body encodes the selected model at the
/// JSON top level (so pass-through must write the mapped `model` back into the
/// copied raw payload). Mirrors Go `passThroughBodyNeedsModelPatch`
/// (`pass_through.go:158-181`).
pub fn pass_through_body_needs_model_patch(api_format: ApiFormat) -> bool {
    matches!(
        api_format,
        ApiFormat::OpenAiChatCompletions
            | ApiFormat::OpenAiResponses
            | ApiFormat::OpenAiResponsesCompact
            | ApiFormat::OpenAiEmbeddings
            | ApiFormat::JinaEmbeddings
            | ApiFormat::JinaRerank
            | ApiFormat::AnthropicMessages
            | ApiFormat::OpenAiAudioSpeech
    )
}

/// S16 - Pure port of Go `mergePassThroughRequestBody`
/// (`pass_through.go:117-135`).
///
/// `[Faraday ?]`: Go's `sjson.SetBytes(body, "model", model)` writes `model`
/// as a JSON string. We mirror that by parsing the body as a
/// `serde_json::Value`, setting `model` to a JSON string, and re-serializing.
/// This re-serializes the entire body (key ordering / whitespace may shift),
/// but JSON object key ordering is not semantically significant. The Go test
/// reads the result back with `gjson.GetBytes`, which is order-independent.
pub fn merge_pass_through_request_body(
    raw_body: &[u8],
    api_format: ApiFormat,
    model: &str,
) -> Result<Vec<u8>, String> {
    let mut body: Value = if raw_body.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_slice(raw_body).map_err(|err| format!("parse pass-through body: {err}"))?
    };

    if !pass_through_body_needs_model_patch(api_format) || model.is_empty() {
        // Skip the model patch entirely - return the cloned inbound body
        // verbatim, matching Go which only mutates bytes when it actually
        // calls sjson.SetBytes. Re-serializing would shift key ordering
        // (serde_json::Map sorts keys alphabetically; Go preserves insertion
        // order), so we preserve the original bytes.
        return Ok(raw_body.to_vec());
    }

    // Set `model` at the JSON top level as a JSON string.
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".to_string(), Value::String(model.to_string()));
    } else {
        // Non-object root: Go's sjson would error too (cannot set key on
        // non-object). Mirror by returning an error so the wiring layer keeps
        // the outbound body.
        return Err(format!(
            "cannot set model on non-object body (root kind: {})",
            json_root_kind(&body)
        ));
    }

    serde_json::to_vec(&body).map_err(|err| format!("re-serialize body: {err}"))
}

fn json_root_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Outcome of [`apply_pass_through_request_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassThroughRequestBodyPlan {
    /// The body bytes to send to the provider. When pass-through is applied
    /// this is the merged inbound body; otherwise it is the original outbound
    /// body unchanged.
    pub body: Vec<u8>,
    /// `true` when the merged inbound body replaced the outbound body (Go sets
    /// `outbound.state.PassThroughApplied = true`).
    pub pass_through_applied: bool,
}

/// S16 - Pure decision + body merge mirroring Go
/// `applyPassThroughRequestBody.OnRawRequest` (`pass_through.go:65-113`).
///
/// Decision:
/// - When disabled or the format is multipart, the plan keeps `outbound_body`
///   and sets `pass_through_applied = false`.
/// - When enabled and the merge succeeds, the plan carries the merged body and
///   `pass_through_applied = true`.
/// - When enabled but the merge fails, the plan keeps `outbound_body` and sets
///   `pass_through_applied = false` (Go logs a warning and continues).
pub fn apply_pass_through_request_body(
    pass_through_enabled: bool,
    api_format: ApiFormat,
    inbound_raw_body: &[u8],
    outbound_body: &[u8],
    model: &str,
) -> PassThroughRequestBodyPlan {
    if !pass_through_enabled || !pass_through_body_supported(api_format) {
        return PassThroughRequestBodyPlan {
            body: outbound_body.to_vec(),
            pass_through_applied: false,
        };
    }

    match merge_pass_through_request_body(inbound_raw_body, api_format, model) {
        Ok(merged) => PassThroughRequestBodyPlan {
            body: merged,
            pass_through_applied: true,
        },
        Err(_) => PassThroughRequestBodyPlan {
            body: outbound_body.to_vec(),
            pass_through_applied: false,
        },
    }
}

// ===========================================================================
// S17 - applyOverrideRequestBody (Go override.go:111-208)
// ===========================================================================

/// Outcome of [`apply_override_request_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideRequestBodyPlan {
    /// The body bytes after applying the override operations (or the original
    /// body when no operation applies).
    pub body: Vec<u8>,
    /// The number of operations that were actually applied (condition passed
    /// AND the op succeeded). Mirrors Go applied-body-override-operations
    /// debug log count.
    pub applied_count: usize,
}

/// S17 - Pure port of Go applyOverrideRequestBody.OnRawRequest +
/// applyBodyOperation (override.go:111-208).
///
/// Each op is dispatched on op.op:
/// - set (with __CONDUIT_CLEAR__ -> delete),
/// - delete,
/// - rename / copy,
/// - array ops (array_append/array_prepend/array_insert/array_remove).
///
/// Path semantics mirror Go gjson/sjson dotted paths for the cases the Go
/// tests exercise (top-level keys, one-level nested like function.name,
/// metadata.original_model). Multi-level wildcards / # array queries are
/// NOT supported (Go gjson query syntax is rich; full port is out of scope
/// for this pure-helper pass).
///
/// [Faraday ?] caveat: condition evaluation re-uses evaluate_condition /
/// render_template, which implement only the {{.Field}} subset (see S19
/// [Pascal ?] caveat). Override-body tests that use {{eq .Model "x"}} or
/// {{index .Metadata "k"}} will not resolve to the expected value - flagged
/// for the parity auditor.
///
/// body is parsed once as a serde_json::Value; on parse failure every op is
/// a no-op (Go would error per-op and log a warning). The result is
/// re-serialized; key ordering may shift but JSON object ordering is not
/// semantically significant.
pub fn apply_override_request_body(
    body: &[u8],
    operations: &[OverrideOperation],
    render_ctx: &RenderContext,
) -> OverrideRequestBodyPlan {
    if operations.is_empty() {
        return OverrideRequestBodyPlan {
            body: body.to_vec(),
            applied_count: 0,
        };
    }

    let mut root: Value = if body.is_empty() {
        Value::Object(serde_json::Map::new())
    } else {
        match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => {
                return OverrideRequestBodyPlan {
                    body: body.to_vec(),
                    applied_count: 0,
                };
            }
        }
    };

    let mut applied = 0usize;
    for op in operations {
        if op.path.eq_ignore_ascii_case("stream") {
            continue;
        }
        if !evaluate_condition(&op.condition, render_ctx) {
            continue;
        }
        let ok = apply_body_operation(&mut root, op, render_ctx);
        if ok {
            applied += 1;
        }
    }

    match serde_json::to_vec(&root) {
        Ok(bytes) => OverrideRequestBodyPlan {
            body: bytes,
            applied_count: applied,
        },
        Err(_) => OverrideRequestBodyPlan {
            body: body.to_vec(),
            applied_count: 0,
        },
    }
}

/// Apply a single OverrideOperation to a parsed body root. Returns true
/// when the op succeeded (mirrors Go no-error path). Errors (path not found,
/// non-array target, bad index) return false (Go logs a warning and
/// continues with the unchanged body).
fn apply_body_operation(root: &mut Value, op: &OverrideOperation, ctx: &RenderContext) -> bool {
    match op.op.as_str() {
        override_op::SET => apply_body_set(root, op, ctx),
        override_op::DELETE => apply_body_delete(root, op),
        override_op::RENAME => apply_body_rename(root, op),
        override_op::COPY => apply_body_copy(root, op),
        override_op::ARRAY_APPEND => apply_body_array_insert(root, op, ctx, ArrayInsertMode::End),
        override_op::ARRAY_PREPEND => {
            apply_body_array_insert(root, op, ctx, ArrayInsertMode::Start)
        }
        override_op::ARRAY_INSERT => {
            apply_body_array_insert(root, op, ctx, ArrayInsertMode::AtIndex)
        }
        override_op::ARRAY_REMOVE => apply_body_array_remove(root, op),
        _ => false,
    }
}

/// Go applyBodySet. __CONDUIT_CLEAR__ -> delete; otherwise set the value at
/// path. The rendered value is parsed as JSON when it looks like a
/// structured value (object/array/number/bool/null), mirroring Go
/// renderOverrideValue.
fn apply_body_set(root: &mut Value, op: &OverrideOperation, ctx: &RenderContext) -> bool {
    let rendered = render_template(&op.value, ctx);
    if rendered == "__CONDUIT_CLEAR__" {
        return apply_body_delete(root, op);
    }
    let value = parse_rendered_value(&rendered);
    dotted_set(root, &op.path, value)
}

/// Go applyBodyDelete.
fn apply_body_delete(root: &mut Value, op: &OverrideOperation) -> bool {
    dotted_delete(root, &op.path)
}

/// Go applyBodyRename. Reads from, deletes it, sets to to the read value.
fn apply_body_rename(root: &mut Value, op: &OverrideOperation) -> bool {
    if let Some(value) = dotted_get(root, &op.from).cloned()
        && dotted_delete(root, &op.from)
    {
        return dotted_set(root, &op.to, value);
    }
    false
}

/// Go applyBodyCopy. Reads from (without deleting) and sets to.
fn apply_body_copy(root: &mut Value, op: &OverrideOperation) -> bool {
    if let Some(value) = dotted_get(root, &op.from).cloned() {
        return dotted_set(root, &op.to, value);
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayInsertMode {
    Start,
    End,
    AtIndex,
}

/// Go applyBodyArrayInsert. Inserts one or more values into the array at
/// op.path. When the rendered value is a JSON array and splat != false,
/// the array elements are spread.
fn apply_body_array_insert(
    root: &mut Value,
    op: &OverrideOperation,
    ctx: &RenderContext,
    mode: ArrayInsertMode,
) -> bool {
    if op.path.is_empty() {
        return false;
    }

    let rendered = render_template(&op.value, ctx);
    let rendered_value = parse_rendered_value(&rendered);

    let splat = op.splat.unwrap_or(true);
    let to_insert: Vec<Value> = match (&rendered_value, splat) {
        (Value::Array(items), true) => items.clone(),
        (other, _) => vec![other.clone()],
    };

    let existing = dotted_get(root, &op.path).cloned();

    let new_array = match existing {
        None => Value::Array(to_insert),
        Some(Value::Array(mut current)) => {
            let pos = match mode {
                ArrayInsertMode::Start => 0,
                ArrayInsertMode::End => current.len(),
                ArrayInsertMode::AtIndex => {
                    let Some(idx) = op.index else {
                        return false;
                    };
                    clamp_insert_index(idx, current.len())
                }
            };
            let mut merged = Vec::with_capacity(current.len() + to_insert.len());
            let tail = current.split_off(pos);
            merged.extend(current);
            merged.extend(to_insert);
            merged.extend(tail);
            Value::Array(merged)
        }
        Some(_) => return false,
    };

    dotted_set(root, &op.path, new_array)
}

/// Go applyBodyArrayRemove. Removes array items whose relative match.path
/// equals match.eq.
fn apply_body_array_remove(root: &mut Value, op: &OverrideOperation) -> bool {
    if op.path.is_empty() {
        return false;
    }
    let Some(match_spec) = &op.r#match else {
        return false;
    };
    if match_spec.path.trim().is_empty() || match_spec.eq.trim().is_empty() {
        return false;
    }

    let Some(Value::Array(items)) = dotted_get(root, &op.path).cloned() else {
        return false;
    };

    let match_eq = match_spec.eq.trim();
    let kept: Vec<Value> = items
        .into_iter()
        .filter(|item| {
            if let Some(result) = dotted_get(item, &match_spec.path) {
                let as_str = match result {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                return as_str != match_eq;
            }
            true
        })
        .collect();

    dotted_set(root, &op.path, Value::Array(kept))
}

/// Clamp a Go array_insert index to [0, len]. Negative indices count from
/// the end (-1 = before last).
fn clamp_insert_index(index: i64, len: usize) -> usize {
    let i_len = len as i64;
    let mut pos = index;
    if pos < 0 {
        pos += i_len;
    }
    if pos < 0 {
        pos = 0;
    }
    if pos > i_len {
        pos = i_len;
    }
    pos as usize
}

/// Parse a rendered template string into a JSON value, mirroring Go
/// renderOverrideValue. Bare JSON-looking strings (objects/arrays/numbers/
/// bools/null) are decoded; everything else is kept as a JSON string.
fn parse_rendered_value(rendered: &str) -> Value {
    let trimmed = rendered.trim();
    if trimmed.is_empty() {
        return Value::String(rendered.to_string());
    }

    let first = trimmed.as_bytes()[0];
    let looks_structured = first == STRUCT_BRACE
        || first == STRUCT_BRACKET
        || first.is_ascii_digit()
        || first == STRUCT_DASH;
    let looks_literal = trimmed == "true" || trimmed == "false" || trimmed == "null";

    if (looks_structured || looks_literal)
        && let Ok(value) = serde_json::from_str::<Value>(trimmed)
    {
        return value;
    }

    Value::String(rendered.to_string())
}

const STRUCT_BRACE: u8 = 0x7Bu8; // `{`
const STRUCT_BRACKET: u8 = 0x5Bu8; // `[`
const STRUCT_DASH: u8 = 0x2Du8; // `-`

// ---------------------------------------------------------------------------
// Dotted-path helpers (subset of gjson/sjson dotted-path semantics)
// ---------------------------------------------------------------------------

/// Walk a dotted path (a.b.c) through nested JSON objects. Array index
/// segments (numeric keys) are also walked when present. Returns a reference
/// to the value at the path, or None.
///
/// [Faraday ?] caveat: only plain object key chains and numeric array indices
/// are supported. gjson query syntax (#, .#, wildcards) is out of scope.
fn dotted_get<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in path.split('.') {
        match current {
            Value::Object(map) => {
                current = map.get(segment)?;
            }
            Value::Array(items) => {
                let idx: usize = segment.parse().ok()?;
                let value = items.get(idx)?;
                current = value;
            }
            _ => return None,
        }
    }
    Some(current)
}

/// Set the value at a dotted path, creating intermediate objects as needed.
/// Mirrors sjson.SetBytes(body, path, value) for object paths.
///
/// [Faraday ?] caveat: array-index path segments are NOT created when absent.
/// The Go override-body tests never write to such paths, so this restriction
/// is sufficient for the test-mirrored paths.
fn dotted_set(root: &mut Value, path: &str, value: Value) -> bool {
    if path.is_empty() {
        *root = value;
        return true;
    }
    let segments: Vec<&str> = path.split('.').collect();
    if segments.len() == 1 {
        return set_on_object(root, segments[0], value);
    }

    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        let next = match current {
            Value::Object(map) => map
                .entry((*segment).to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new())),
            _ => return false,
        };
        current = next;
    }
    let last = segments[segments.len() - 1];
    set_on_object(current, last, value)
}

fn set_on_object(value: &mut Value, key: &str, new_value: Value) -> bool {
    match value {
        Value::Object(map) => {
            map.insert(key.to_string(), new_value);
            true
        }
        _ => false,
    }
}

/// Delete the value at a dotted path. Returns true when something was
/// deleted. Mirrors sjson.DeleteBytes(body, path).
fn dotted_delete(root: &mut Value, path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let segments: Vec<&str> = path.split('.').collect();
    if segments.len() == 1 {
        return delete_from_object(root, segments[0]);
    }

    let mut current = root;
    for segment in &segments[..segments.len() - 1] {
        match current {
            Value::Object(map) => match map.get_mut(*segment) {
                Some(child) => current = child,
                None => return false,
            },
            Value::Array(items) => {
                let Ok(idx) = segment.parse::<usize>() else {
                    return false;
                };
                match items.get_mut(idx) {
                    Some(child) => current = child,
                    None => return false,
                }
            }
            _ => return false,
        }
    }
    let last = segments[segments.len() - 1];
    delete_from_object(current, last)
}

fn delete_from_object(value: &mut Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.remove(key).is_some(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::ApiFormat;

    // 解析 plan.body 为 JSON Value；失败则 panic（测试 helper，绕过 unwrap_used deny）。
    fn parse_body(body: &[u8]) -> Value {
        match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => panic!("failed to parse body as JSON: {e}"),
        }
    }

    // ---------- S13 applyPassThroughResponse ----------

    #[test]
    fn s13_disabled_returns_transformed() {
        let plan = apply_pass_through_response(false, true);
        assert!(plan.is_transformed());
    }

    #[test]
    fn s13_enabled_but_no_raw_response_returns_transformed() {
        let plan = apply_pass_through_response(true, false);
        assert!(plan.is_transformed());
    }

    #[test]
    fn s13_enabled_with_raw_response_returns_raw() {
        let plan = apply_pass_through_response(true, true);
        assert!(plan.is_raw());
    }

    // Mirrors Go TestApplyPassThroughResponse_Disabled.
    #[test]
    fn s13_go_disabled() {
        let inputs = PassThroughInputs {
            has_channel: true,
            has_raw_provider_request: true,
            api_formats_match: true,
            streams_aligned: true,
            pass_through_body_enabled: false,
        };
        let plan = apply_pass_through_response(is_pass_through_enabled(&inputs), true);
        assert!(plan.is_transformed());
    }

    // Mirrors Go TestApplyPassThroughResponse_Enabled_ReturnsRaw.
    #[test]
    fn s13_go_enabled_returns_raw() {
        let inputs = PassThroughInputs {
            has_channel: true,
            has_raw_provider_request: true,
            api_formats_match: true,
            streams_aligned: true,
            pass_through_body_enabled: true,
        };
        let plan = apply_pass_through_response(is_pass_through_enabled(&inputs), true);
        assert!(plan.is_raw());
    }

    // Mirrors Go TestApplyPassThroughResponse_MismatchedAPIFormat.
    #[test]
    fn s13_go_mismatched_api_format() {
        let inputs = PassThroughInputs {
            has_channel: true,
            has_raw_provider_request: true,
            api_formats_match: false,
            streams_aligned: true,
            pass_through_body_enabled: true,
        };
        let plan = apply_pass_through_response(is_pass_through_enabled(&inputs), true);
        assert!(plan.is_transformed());
    }

    // Mirrors Go TestApplyPassThroughResponse_NilLlmRequest.
    #[test]
    fn s13_go_nil_llm_request() {
        let inputs = PassThroughInputs {
            has_channel: true,
            has_raw_provider_request: false,
            api_formats_match: false,
            streams_aligned: false,
            pass_through_body_enabled: true,
        };
        let plan = apply_pass_through_response(is_pass_through_enabled(&inputs), true);
        assert!(plan.is_transformed());
    }

    // ---------- S14 applyPassThroughStream ----------

    #[test]
    fn s14_disabled_returns_transformed() {
        let plan = apply_pass_through_stream(false, true);
        assert!(plan.is_transformed());
    }

    #[test]
    fn s14_enabled_but_no_raw_stream_returns_transformed() {
        let plan = apply_pass_through_stream(true, false);
        assert!(plan.is_transformed());
    }

    #[test]
    fn s14_enabled_with_raw_stream_returns_raw() {
        let plan = apply_pass_through_stream(true, true);
        assert!(plan.is_raw());
    }

    // Mirrors Go TestApplyPassThroughStream_Disabled.
    #[test]
    fn s14_go_disabled() {
        let inputs = PassThroughInputs {
            has_channel: true,
            pass_through_body_enabled: false,
            ..Default::default()
        };
        let plan = apply_pass_through_stream(is_pass_through_enabled(&inputs), false);
        assert!(plan.is_transformed());
    }

    // Mirrors Go TestApplyPassThroughStream_NoRawChannel.
    #[test]
    fn s14_go_no_raw_channel() {
        let inputs = PassThroughInputs {
            has_channel: true,
            has_raw_provider_request: true,
            api_formats_match: true,
            streams_aligned: true,
            pass_through_body_enabled: true,
        };
        let plan = apply_pass_through_stream(is_pass_through_enabled(&inputs), false);
        assert!(plan.is_transformed());
    }

    // ---------- passThroughStreamAligned (mirrors 5 Go golden tests) ----------

    #[test]
    fn s13_stream_aligned_supported_parameter_changed_to_false() {
        assert!(!pass_through_stream_aligned(None, true));
    }

    #[test]
    fn s13_stream_aligned_gemini_forced_stream_mismatch() {
        assert!(!pass_through_stream_aligned(None, true));
    }

    #[test]
    fn s13_stream_aligned_nil_and_false_align() {
        assert!(pass_through_stream_aligned(None, false));
    }

    #[test]
    fn s13_stream_aligned_original_false_forced_true_misaligns() {
        assert!(!pass_through_stream_aligned(Some(false), true));
    }

    #[test]
    fn s13_stream_aligned_both_true_align() {
        assert!(pass_through_stream_aligned(Some(true), true));
    }

    // ---------- S16 passThroughBodySupported / NeedsModelPatch ----------

    #[test]
    fn s16_body_supported_excludes_multipart() {
        assert!(!pass_through_body_supported(
            ApiFormat::OpenAiAudioTranscriptions
        ));
        assert!(!pass_through_body_supported(
            ApiFormat::OpenAiAudioTranslations
        ));
        assert!(!pass_through_body_supported(ApiFormat::OpenAiImageEdit));
        assert!(!pass_through_body_supported(
            ApiFormat::OpenAiImageVariation
        ));
    }

    #[test]
    fn s16_body_supported_includes_chat() {
        assert!(pass_through_body_supported(
            ApiFormat::OpenAiChatCompletions
        ));
        assert!(pass_through_body_supported(ApiFormat::AnthropicMessages));
        assert!(pass_through_body_supported(ApiFormat::OpenAiAudioSpeech));
    }

    #[test]
    fn s16_needs_model_patch_chat_yes() {
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::OpenAiChatCompletions
        ));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::OpenAiResponses
        ));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::OpenAiResponsesCompact
        ));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::OpenAiEmbeddings
        ));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::JinaEmbeddings
        ));
        assert!(pass_through_body_needs_model_patch(ApiFormat::JinaRerank));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::AnthropicMessages
        ));
        assert!(pass_through_body_needs_model_patch(
            ApiFormat::OpenAiAudioSpeech
        ));
    }

    #[test]
    fn s16_needs_model_patch_gemini_contents_no() {
        assert!(!pass_through_body_needs_model_patch(
            ApiFormat::GeminiContents
        ));
        assert!(!pass_through_body_needs_model_patch(
            ApiFormat::OpenAiAudioTranscriptions
        ));
    }

    // ---------- S16 mergePassThroughRequestBody ----------

    #[test]
    fn s16_merge_preserves_mapped_model_openai_chat() -> Result<(), String> {
        let raw = br#"{"model":"my-alias","messages":[],"temperature":0.4}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "gpt-4o")?;
        let v: Value = serde_json::from_slice(&merged).map_err(|e| e.to_string())?;
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["temperature"], 0.4);
        Ok(())
    }

    #[test]
    fn s16_merge_preserves_mapped_model_jina_rerank() -> Result<(), String> {
        let raw = br#"{"model":"Qwen-3-Rerank-8B","query":"q","documents":["a","b"],"top_n":2}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::JinaRerank, "Qwen/Qwen3-Reranker-8B")?;
        let v: Value = serde_json::from_slice(&merged).map_err(|e| e.to_string())?;
        assert_eq!(v["model"], "Qwen/Qwen3-Reranker-8B");
        assert_eq!(v["top_n"], 2);
        Ok(())
    }

    #[test]
    fn s16_merge_preserves_mapped_model_jina_embedding() -> Result<(), String> {
        let raw = br#"{"model":"my-embedding-alias","input":"hello","task":"retrieval.query"}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::JinaEmbeddings, "jina-embeddings-v3")?;
        let v: Value = serde_json::from_slice(&merged).map_err(|e| e.to_string())?;
        assert_eq!(v["model"], "jina-embeddings-v3");
        assert_eq!(v["task"], "retrieval.query");
        Ok(())
    }

    #[test]
    fn s16_merge_skips_model_patch_for_gemini_contents() -> Result<(), String> {
        let raw = br#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::GeminiContents, "gemini-2.5-pro")?;
        assert_eq!(merged, raw.to_vec());
        Ok(())
    }

    #[test]
    fn s16_merge_applies_speech_model_patch() -> Result<(), String> {
        let raw = br#"{"model":"my-tts-alias","input":"hello","voice":"alloy"}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::OpenAiAudioSpeech, "tts-1-hd")?;
        let v: Value = serde_json::from_slice(&merged).map_err(|e| e.to_string())?;
        assert_eq!(v["model"], "tts-1-hd");
        assert_eq!(v["voice"], "alloy");
        Ok(())
    }

    #[test]
    fn s16_merge_empty_model_skips_patch() -> Result<(), String> {
        let raw = br#"{"model":"x","temperature":0.5}"#;
        let merged = merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "")?;
        let v: Value = serde_json::from_slice(&merged).map_err(|e| e.to_string())?;
        assert_eq!(v["model"], "x");
        Ok(())
    }

    #[test]
    fn s16_merge_non_object_root_errors() {
        let raw = br#"[1,2,3]"#;
        let result =
            merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "gpt-4o");
        assert!(result.is_err());
    }

    // ---------- S16 apply_pass_through_request_body ----------

    #[test]
    fn s16_apply_passes_through_with_model_patch() {
        let inbound = br#"{"model":"my-alias","messages":[],"temperature":0.4}"#;
        let outbound = br#"{"model":"gpt-4o","messages":[]}"#;
        let plan = apply_pass_through_request_body(
            true,
            ApiFormat::OpenAiChatCompletions,
            inbound,
            outbound,
            "gpt-4o",
        );
        assert!(plan.pass_through_applied);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["model"], "gpt-4o");
        assert_eq!(v["temperature"], 0.4);
    }

    #[test]
    fn s16_apply_skips_multipart_formats() {
        for format in [
            ApiFormat::OpenAiAudioTranscriptions,
            ApiFormat::OpenAiAudioTranslations,
            ApiFormat::OpenAiImageEdit,
            ApiFormat::OpenAiImageVariation,
        ] {
            let inbound = b"--client-boundary\r\n\r\n--client-boundary--\r\n";
            let outbound = b"--new-boundary\r\n\r\n--new-boundary--\r\n";
            let plan =
                apply_pass_through_request_body(true, format, inbound, outbound, "mapped-model");
            assert!(!plan.pass_through_applied);
            assert_eq!(plan.body, outbound.to_vec());
        }
    }

    #[test]
    fn s16_apply_skips_when_disabled() {
        let inbound = br#"{"model":"my-alias"}"#;
        let outbound = br#"{"model":"gpt-4o","stream":true}"#;
        let plan = apply_pass_through_request_body(
            false,
            ApiFormat::OpenAiChatCompletions,
            inbound,
            outbound,
            "gpt-4o",
        );
        assert!(!plan.pass_through_applied);
        assert_eq!(plan.body, outbound.to_vec());
    }

    #[test]
    fn s16_apply_falls_back_on_merge_error() {
        let inbound = b"{not json}";
        let outbound = br#"{"model":"gpt-4o"}"#;
        let plan = apply_pass_through_request_body(
            true,
            ApiFormat::OpenAiChatCompletions,
            inbound,
            outbound,
            "gpt-4o",
        );
        assert!(!plan.pass_through_applied);
        assert_eq!(plan.body, outbound.to_vec());
    }

    // ---------- S17 set / delete / clear ----------

    fn empty_ctx() -> RenderContext {
        RenderContext::default()
    }

    fn ctx_with_model(model: &str) -> RenderContext {
        RenderContext {
            model: model.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn s17_empty_ops_no_change() {
        let body = br#"{"model":"x"}"#;
        let plan = apply_override_request_body(body, &[], &empty_ctx());
        assert_eq!(plan.applied_count, 0);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["model"], "x");
    }

    #[test]
    fn s17_set_top_level_string() {
        let body = br#"{"temperature":0.3}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "temperature".to_string(),
            value: "0.9".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["temperature"], 0.9);
    }

    #[test]
    fn s17_set_top_level_object_via_template() {
        let body = br#"{"messages":[]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "system".to_string(),
            value: r#"{"role":"system","content":"be nice"}"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["system"]["role"], "system");
        assert_eq!(v["system"]["content"], "be nice");
    }

    #[test]
    fn s17_set_clear_sentinel_deletes() {
        let body = br#"{"temperature":0.3,"top_p":0.9}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "temperature".to_string(),
            value: "__CONDUIT_CLEAR__".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert!(
            !v.as_object()
                .map_or(false, |o| o.contains_key("temperature"))
        );
        assert_eq!(v["top_p"], 0.9);
    }

    #[test]
    fn s17_delete_op() {
        let body = br#"{"a":1,"b":2}"#;
        let ops = vec![OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "a".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert!(!v.as_object().map_or(false, |o| o.contains_key("a")));
        assert_eq!(v["b"], 2);
    }

    // ---------- S17 rename / copy ----------

    #[test]
    fn s17_rename_moves_top_level_key() {
        let body = br#"{"old_key":"v","keep":1}"#;
        let ops = vec![OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "old_key".to_string(),
            to: "new_key".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert!(!v.as_object().map_or(false, |o| o.contains_key("old_key")));
        assert_eq!(v["new_key"], "v");
        assert_eq!(v["keep"], 1);
    }

    #[test]
    fn s17_rename_missing_from_is_noop() {
        let body = br#"{"keep":1}"#;
        let ops = vec![OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "missing".to_string(),
            to: "x".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 0);
    }

    #[test]
    fn s17_copy_preserves_source() {
        let body = br#"{"src":"v","other":1}"#;
        let ops = vec![OverrideOperation {
            op: override_op::COPY.to_string(),
            from: "src".to_string(),
            to: "dst".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["src"], "v");
        assert_eq!(v["dst"], "v");
    }

    #[test]
    fn s17_set_nested_dotted_path() {
        let body = br#"{"config":{"model":"old"}}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "config.model".to_string(),
            value: "new-model".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["config"]["model"], "new-model");
    }

    #[test]
    fn s17_stream_path_is_ignored() {
        let body = br#"{"stream":false,"model":"x"}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "STREAM".to_string(),
            value: "true".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 0);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["stream"], false);
    }

    // ---------- S17 array ops ----------

    #[test]
    fn s17_array_append_single_object() {
        let body = br#"{"system":[{"type":"text","text":"original"}]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "system".to_string(),
            value: r#"{"type":"text","text":"injected"}"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["system"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "original");
        assert_eq!(arr[1]["text"], "injected");
    }

    #[test]
    fn s17_array_prepend_single_object() {
        let body = br#"{"system":[{"type":"text","text":"original"}]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_PREPEND.to_string(),
            path: "system".to_string(),
            value: r#"{"type":"text","text":"injected"}"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["system"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr[0]["text"], "injected");
        assert_eq!(arr[1]["text"], "original");
    }

    #[test]
    fn s17_array_prepend_splat_default_true() {
        let body = br#"{"system":[{"text":"original"}]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_PREPEND.to_string(),
            path: "system".to_string(),
            value: r#"[{"text":"a"},{"text":"b"}]"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["system"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["text"], "a");
        assert_eq!(arr[1]["text"], "b");
        assert_eq!(arr[2]["text"], "original");
    }

    #[test]
    fn s17_array_prepend_splat_false_nests_array() {
        let body = br#"{"tags":["x"]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_PREPEND.to_string(),
            path: "tags".to_string(),
            value: r#"["a","b"]"#.to_string(),
            splat: Some(false),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["tags"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr.len(), 2);
        assert!(arr[0].is_array());
        assert_eq!(arr[0][0], "a");
        assert_eq!(arr[0][1], "b");
        assert_eq!(arr[1], "x");
    }

    #[test]
    fn s17_array_insert_positive_index() {
        let body = br#"{"items":["a","b","c"]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            index: Some(1),
            value: "X".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["items"], serde_json::json!(["a", "X", "b", "c"]));
    }

    #[test]
    fn s17_array_insert_negative_index() {
        let body = br#"{"items":["a","b","c"]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            index: Some(-1),
            value: "X".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["items"], serde_json::json!(["a", "b", "X", "c"]));
    }

    #[test]
    fn s17_array_insert_clamps_high_index() {
        let body = br#"{"items":["a","b"]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            index: Some(99),
            value: "X".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["items"], serde_json::json!(["a", "b", "X"]));
    }

    #[test]
    fn s17_array_append_missing_path_creates_array() {
        let body = br#"{}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "system".to_string(),
            value: r#"{"type":"text","text":"only"}"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["system"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["text"], "only");
    }

    #[test]
    fn s17_array_prepend_non_array_noop() {
        let body = br#"{"system":"not-an-array"}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_PREPEND.to_string(),
            path: "system".to_string(),
            value: "X".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 0);
        let v: Value = parse_body(&plan.body);
        assert_eq!(v["system"], "not-an-array");
    }

    #[test]
    fn s17_array_remove_by_nested_name() {
        let body = br#"{"tools":[{"type":"function","function":{"name":"get_weather"}},{"type":"function","function":{"name":"web_search"}},{"type":"function","function":{"name":"calculate"}}]}"#;
        let ops = vec![OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "tools".to_string(),
            r#match: Some(conduit_core::objects::overrides::OverrideMatch {
                path: "function.name".to_string(),
                eq: "web_search".to_string(),
            }),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 1);
        let v: Value = parse_body(&plan.body);
        let arr = v["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("expected array"));
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["function"]["name"], "get_weather");
        assert_eq!(arr[1]["function"]["name"], "calculate");
    }

    // ---------- S17 conditions / malformed body ----------

    #[test]
    fn s17_condition_empty_executes() {
        let body = br#"{"x":1}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "x".to_string(),
            value: "2".to_string(),
            condition: String::new(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &ctx_with_model("anything"));
        assert_eq!(plan.applied_count, 1);
    }

    #[test]
    fn s17_condition_non_empty_non_resolved_skips() {
        // {{eq .Model "x"}} is NOT supported by the simple template engine -
        // known parity gap flagged with [Faraday ?].
        let body = br#"{"x":1}"#;
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "x".to_string(),
            value: "2".to_string(),
            condition: r#"{{eq .Model "x"}}"#.to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &ctx_with_model("x"));
        assert_eq!(plan.applied_count, 0);
    }

    #[test]
    fn s17_malformed_body_keeps_bytes_verbatim() {
        let body = b"{not json}";
        let ops = vec![OverrideOperation {
            op: override_op::SET.to_string(),
            path: "x".to_string(),
            value: "1".to_string(),
            ..Default::default()
        }];
        let plan = apply_override_request_body(body, &ops, &empty_ctx());
        assert_eq!(plan.applied_count, 0);
        assert_eq!(plan.body, body.to_vec());
    }
}

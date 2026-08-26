//! Jina inbound transformer — pure-logic primitives for the Jina-compatible
//! embedding + rerank inbound surface.
//!
//! Mirrors Go `conduit/llm/transformer/jina/{embedding,rerank}_inbound.go`.
//! Pure primitives implemented here (S04/S06/S07/S09/S10/S11):
//! - [`parse_jina_route`]           — S04/S08 dual-entry path classifier
//! - [`validate_jina_input`]        — S06 embedding input validation
//! - [`validate_jina_batch_size`]   — S11 batch input size cap (Rust-side
//!                                    hardening; no direct Go counterpart)
//! - [`reject_stream`]              — S07 no-stream gate
//! - [`resolve_embedding_usage`]    — S09 embeddings usage forwarding
//! - [`build_rerank_response`]      — S10 rerank response field casing
//!
//! No I/O, no HTTP wiring.

use conduit_core::ConduitError;
use conduit_llm::Usage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TransformerResult;

/// Jina route kind returned by [`parse_jina_route`].
///
/// Mirrors the inbound handlers wired in `conduit/internal/server/routes.go`
/// lines 176 / 192 / 196-198 plus `api/jina.go`:
/// - `/v1/rerank` and `/jina/v1/rerank` → `Rerank`
/// - `/jina/v1/embeddings`              → `Embedding` (Jina-native)
/// - `/v1/embeddings`                   → routed to the **OpenAI** inbound
///   handler in Go (line 176), so it is classified as embedding but flagged
///   `jina_native = false` so callers dispatch to the OpenAI transformer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JinaRoute {
    pub kind: JinaRouteKind,
    /// `true` for the `/jina/v1/*` mount (Jina-native inbound transformer).
    /// `false` for the OpenAI-compatible `/v1/*` mount.
    pub jina_native: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JinaRouteKind {
    Embedding,
    Rerank,
}

impl JinaRoute {
    fn new(kind: JinaRouteKind, jina_native: bool) -> Self {
        Self { kind, jina_native }
    }
}

/// Classify an inbound request path into a Jina embedding/rerank route.
///
/// Mirrors the route table at `conduit/internal/server/routes.go`:
/// ```text
/// openaiGroup.POST("/rerank",       handlers.Jina.Rerank)          // line 192
/// jinaGroup := apiGroup.Group("/jina/v1")                          // line 196
/// jinaGroup.POST("/embeddings",     handlers.Jina.CreateEmbedding) // line 197
/// jinaGroup.POST("/rerank",         handlers.Jina.Rerank)          // line 198
/// ```
/// Returns `None` for paths that are not part of the Jina surface (so the
/// caller can fall through to the OpenAI / Anthropic / Gemini routers).
///
/// Note: `/v1/embeddings` is served by the **OpenAI** handler in Go (line
/// 176), not the Jina handler. We recognize it as an embedding entry but
/// flag `jina_native = false` so the caller dispatches to the OpenAI
/// inbound transformer — matching the Go routing.
pub fn parse_jina_route(path: &str) -> Option<JinaRoute> {
    let normalized = path.trim_end_matches('/');

    // Match the Go router behaviour exactly. We match the exact (trailing
    // slash stripped) path rather than splitting on `/`, because these are
    // the relative suffixes that reach the Jina handlers after Gin mounts
    // them under a configurable base.
    match normalized {
        "/jina/v1/embeddings" => Some(JinaRoute::new(JinaRouteKind::Embedding, true)),
        "/jina/v1/rerank" => Some(JinaRoute::new(JinaRouteKind::Rerank, true)),
        "/v1/rerank" => Some(JinaRoute::new(JinaRouteKind::Rerank, false)),
        // OpenAI-compatible embedding mount. In Go this is served by the
        // OpenAI inbound handler, so flag it as non-native (S08 dual-entry
        // distinction).
        "/v1/embeddings" => Some(JinaRoute::new(JinaRouteKind::Embedding, false)),
        _ => None,
    }
}

/// Validate a Jina embedding `input` field.
///
/// Mirrors Go `validateEmbeddingInput` at `jina/embedding_inbound.go` lines
/// 77-119. Accepts the four Go-recognized shapes — string, `[]string`,
/// `[]int` (token ids), `[][]int` (batched token ids) — and returns
/// [`ConduitError::invalid_request`] (HTTP 400, code `invalid_request`) with the
/// same messages the Go test golden cases assert on (see
/// `jina/embedding_test.go`):
/// - empty array            → `"input cannot be empty array"`
/// - empty string in array  → `"input[{i}] cannot be empty string"`
/// - nested empty array     → `"input[{i}] cannot be empty array"`
/// - empty/whitespace scalar → `"input cannot be empty string"`
pub fn validate_jina_input(input: &Value) -> TransformerResult<()> {
    match input {
        Value::Null => Err(ConduitError::invalid_request(
            "input cannot be empty string",
        )),
        Value::String(s) => {
            if s.trim().is_empty() {
                Err(ConduitError::invalid_request(
                    "input cannot be empty string",
                ))
            } else {
                Ok(())
            }
        }
        // Go: `StringArray` branch (string slice). Empty array or any
        // whitespace-only element is rejected.
        Value::Array(arr) if arr.iter().all(Value::is_string) => validate_string_array(arr),
        // Go: `IntArray` branch (token ids). Only the empty-array check
        // applies; individual ints are not validated.
        Value::Array(arr) if arr.iter().all(Value::is_number) => {
            if arr.is_empty() {
                Err(ConduitError::invalid_request("input cannot be empty array"))
            } else {
                Ok(())
            }
        }
        // Go: `IntArrayArray` branch (batched token ids). Empty outer array
        // OR any empty inner array is rejected.
        Value::Array(arr) if arr.iter().all(Value::is_array) => {
            if arr.is_empty() {
                return Err(ConduitError::invalid_request("input cannot be empty array"));
            }
            for (i, inner) in arr.iter().enumerate() {
                let inner_arr = match inner {
                    Value::Array(a) => a,
                    // SAFETY: the match guard above guarantees every element
                    // is an array; this branch is unreachable.
                    _ => unreachable!("match guard guarantees inner is an array"),
                };
                if inner_arr.is_empty() {
                    return Err(ConduitError::invalid_request(format!(
                        "input[{i}] cannot be empty array"
                    )));
                }
            }
            Ok(())
        }
        // Mixed-type or otherwise malformed input: defer to the empty-string
        // rule (matches Go's fallthrough, which treats input as a scalar
        // when none of the union branches match).
        _ => Err(ConduitError::invalid_request(
            "input cannot be empty string",
        )),
    }
}

fn validate_string_array(arr: &[Value]) -> TransformerResult<()> {
    if arr.is_empty() {
        return Err(ConduitError::invalid_request("input cannot be empty array"));
    }
    for (i, v) in arr.iter().enumerate() {
        let s = match v {
            Value::String(s) => s,
            // SAFETY: caller only invokes this when every element is a
            // string; this branch is unreachable.
            _ => unreachable!("caller guarantees all elements are strings"),
        };
        if s.trim().is_empty() {
            return Err(ConduitError::invalid_request(format!(
                "input[{i}] cannot be empty string"
            )));
        }
    }
    Ok(())
}

/// Maximum number of embedding inputs accepted in a single Jina batch
/// request (S11).
///
/// This is a **Rust-side hardening guard** — Go has no direct counterpart
/// and currently lets oversized batches flow through to the provider, which
/// then surfaces a generic upstream error. We cap pre-flight to give the
/// caller a deterministic `ConduitError::InvalidRequest` (HTTP 400) instead.
///
/// The value mirrors OpenAI's documented embeddings batch limit of 2048
/// inputs (OpenAI platform docs, "Limits" section), which Jina's
/// OpenAI-compatible surface inherits. Batches at or below this limit are
/// accepted; anything larger is rejected before any network hop.
pub const JINA_MAX_BATCH_SIZE: usize = 2048;

/// Count the number of embedding inputs in a Jina `input` field, mirroring
/// the four Go-recognized shapes (see [`validate_jina_input`]):
/// - scalar string          → 1
/// - `[]string`             → N
/// - `[]int` (token ids)    → N
/// - `[][]int` (batched)    → N (outer length)
/// - anything else          → 0
pub fn jina_input_count(input: &Value) -> usize {
    match input {
        Value::String(_) => 1,
        Value::Array(arr) => arr.len(),
        _ => 0,
    }
}

/// Reject a Jina embedding batch whose input count exceeds
/// [`JINA_MAX_BATCH_SIZE`] (S11).
///
/// Returns `Ok(())` when `input_count <= JINA_MAX_BATCH_SIZE`, and
/// `Err(ConduitError::InvalidRequest)` (HTTP 400, code `invalid_request`)
/// otherwise. This is a pure helper intended to be called after
/// [`validate_jina_input`] (so the empty-array/empty-string rules are
/// already enforced) but before forwarding to the provider.
pub fn validate_jina_batch_size(input_count: usize) -> TransformerResult<()> {
    if input_count > JINA_MAX_BATCH_SIZE {
        Err(ConduitError::invalid_request(format!(
            "input batch size {input_count} exceeds maximum of {JINA_MAX_BATCH_SIZE}"
        )))
    } else {
        Ok(())
    }
}

/// Reject `stream=true` for embedding/rerank requests.
///
/// Mirrors Go `EmbeddingInboundTransformer.TransformStream`
/// (`jina/embedding_inbound.go:177,184`) and
/// `RerankInboundTransformer.TransformStream`
/// (`jina/rerank_inbound.go:185,193` / `jina/outbound.go:390,398`). The two
/// APIs return *different* byte-exact strings, so the kind discriminant
/// selects the message: embeddings → `"embeddings do not support streaming"`
/// (plural "do"), rerank → `"rerank does not support streaming"` (singular
/// "does"). The `false` value (and absent flag) is accepted.
pub fn reject_stream(kind: JinaRouteKind, stream_flag: Option<bool>) -> TransformerResult<()> {
    if stream_flag.unwrap_or(false) {
        let msg = match kind {
            JinaRouteKind::Embedding => "embeddings do not support streaming",
            JinaRouteKind::Rerank => "rerank does not support streaming",
        };
        Err(ConduitError::invalid_request(msg))
    } else {
        Ok(())
    }
}

/// Resolve the embedding response `usage` to forward downstream (S09).
///
/// Mirrors Go `EmbeddingInboundTransformer.TransformResponse` at
/// `jina/embedding_inbound.go` lines 144-151: when the upstream `llmResp.Usage`
/// is non-nil, the response carries an `EmbeddingUsage` projecting only
/// `PromptTokens` and `TotalTokens` (Go `EmbeddingUsage` struct,
/// `jina/model.go:53-56`); when upstream usage is `nil`, no usage is emitted.
///
/// This helper returns:
/// - `Some(Usage { prompt_tokens, total_tokens, ..zeroed })` when the caller
///   passes `Some`, mirroring Go's two-field projection (completion tokens
///   and token-details are zeroed, matching Go's struct which simply omits
///   them).
/// - `None` when the caller passes `None`, matching Go's nil branch.
pub fn resolve_embedding_usage(llm_usage: Option<&Usage>) -> Option<Usage> {
    llm_usage.map(|u| Usage {
        prompt_tokens: u.prompt_tokens,
        total_tokens: u.total_tokens,
        // Go's EmbeddingUsage only carries prompt/total; mirror that
        // projection by zeroing the remaining fields.
        ..Usage::zero()
    })
}

/// Jina rerank response (S10). Mirrors Go `RerankResponse` /
/// `RerankResult` / `RerankDocument` at `jina/model.go` lines 13-35.
///
/// Field casing is load-bearing: `relevance_score` uses snake_case (the only
/// field in this struct where Go's tag is not camelCase); `index` /
/// `document` / `text` / `model` / `object` / `results` are all single-word
/// lowercase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankResponse {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub results: Vec<RerankResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<RerankUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankResult {
    pub index: i64,
    pub relevance_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<RerankDocument>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankDocument {
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RerankUsage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// A single rerank hit fed into [`build_rerank_response`].
///
/// `document` is optional (Go only emits it when `return_documents=true`).
#[derive(Debug, Clone, PartialEq)]
pub struct RerankHit {
    pub index: i64,
    pub relevance_score: f64,
    pub document: Option<String>,
}

/// Build a Jina-shaped rerank response payload from upstream hits.
///
/// Mirrors Go `RerankInboundTransformer.TransformResponse` at
/// `jina/rerank_inbound.go` lines 77-137: preserves `index`,
/// `relevance_score`, and the optional `document.text` casing exactly.
/// `usage` is forwarded when present.
pub fn build_rerank_response(
    model: &str,
    object: &str,
    hits: &[RerankHit],
    usage: Option<(i64, i64)>,
) -> RerankResponse {
    let results = hits
        .iter()
        .map(|h| RerankResult {
            index: h.index,
            relevance_score: h.relevance_score,
            document: h
                .document
                .as_ref()
                .map(|t| RerankDocument { text: t.clone() }),
        })
        .collect();

    RerankResponse {
        model: model.to_string(),
        object: object.to_string(),
        results,
        usage: usage.map(|(p, t)| RerankUsage {
            prompt_tokens: p,
            total_tokens: t,
        }),
    }
}

// ---- S12: Jina embedding request/response types ----------------------

/// Jina embedding request body (outbound). Mirrors Go `EmbeddingRequest` at
/// `jina/model.go:37-44`. The `input` field accepts the same four Go-recognized
/// shapes (string, `[]string`, `[]int`, `[][]int`) via `serde_json::Value`.
/// `task` / `encoding_format` / `dimensions` / `user` are `omitempty` in Go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub input: Value,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

/// Jina embedding response body. Mirrors Go `EmbeddingResponse` at
/// `jina/model.go:46-51`. Unlike the rerank response, `usage` is always
/// serialized (Go field has no `omitempty`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub data: Vec<EmbeddingData>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub usage: EmbeddingUsage,
}

/// Single embedding data entry. Mirrors Go `EmbeddingData` at
/// `jina/model.go:53-57`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingData {
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub embedding: Vec<f64>,
    #[serde(default)]
    pub index: i64,
}

/// Embedding usage projection. Mirrors Go `EmbeddingUsage` at
/// `jina/model.go:59-62`. Only carries `prompt_tokens` and `total_tokens`
/// (no `completion_tokens` — this is a simpler shape than the full `Usage`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingUsage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

impl EmbeddingUsage {
    pub fn zero() -> Self {
        Self {
            prompt_tokens: 0,
            total_tokens: 0,
        }
    }
}

// ---- S12: pure helpers for embedding URL/task/usage/validation --------

/// Build the Jina embedding API URL from a raw base URL.
///
/// Mirrors Go `OutboundTransformer.buildEmbeddingURL` (outbound.go:246-252)
/// composed with the base-URL normalization at construction time
/// (outbound.go:58-62). When `endpoint_path` is empty (the default),
/// the base URL is normalized with `v1` and `/embeddings` is appended.
/// When `endpoint_path` is set, version normalization is skipped and the
/// custom path is appended directly.
pub fn build_jina_embedding_url(base_url: &str, endpoint_path: &str) -> String {
    let (normalized, path) = if endpoint_path.is_empty() {
        (
            crate::openai_outbound::normalize_base_url(base_url.to_string(), "v1"),
            "/embeddings",
        )
    } else {
        (
            crate::openai_outbound::normalize_base_url(base_url.to_string(), ""),
            endpoint_path,
        )
    };
    format!("{normalized}{path}")
}

/// Resolve the Jina embedding `task` field. When the task is empty, default
/// to `"text-matching"` (Go outbound.go:202-204).
pub fn resolve_jina_embedding_task(task: &str) -> &str {
    if task.is_empty() {
        "text-matching"
    } else {
        task
    }
}

/// Outbound embedding usage extraction gate.
///
/// Mirrors Go `transformEmbeddingResponse` at outbound.go:374:
/// usage is emitted only when `PromptTokens > 0 || TotalTokens > 0`. This
/// differs from the inbound usage projection ([`resolve_embedding_usage`]),
/// which forwards any non-nil usage regardless of field values.
pub fn extract_outbound_embedding_usage(
    prompt_tokens: u64,
    total_tokens: u64,
) -> Option<(u64, u64)> {
    if prompt_tokens > 0 || total_tokens > 0 {
        Some((prompt_tokens, total_tokens))
    } else {
        None
    }
}

/// Validate the HTTP content type for a Jina embedding/rerank request.
///
/// Mirrors Go `EmbeddingInboundTransformer.TransformRequest` at
/// embedding_inbound.go:34-41. Empty/missing content type defaults to
/// `application/json` (accepted); any content type containing
/// `application/json` (case-insensitive) is accepted; everything else is
/// rejected.
pub fn validate_jina_content_type(content_type: &str) -> TransformerResult<()> {
    if content_type.is_empty() || content_type.to_lowercase().contains("application/json") {
        Ok(())
    } else {
        Err(ConduitError::invalid_request(format!(
            "unsupported content type: {content_type}"
        )))
    }
}

/// Validate that the model field is non-empty.
///
/// Mirrors Go embedding_inbound.go:50-52.
pub fn validate_jina_model_required(model: &str) -> TransformerResult<()> {
    if model.is_empty() {
        Err(ConduitError::invalid_request("model is required"))
    } else {
        Ok(())
    }
}

/// Parse a Jina-format error response body and extract the message.
///
/// Mirrors Go `OutboundTransformer.TransformError` at outbound.go:402-437.
/// The Jina error format is `{"error": {"message": "...", "type": "..."}}`.
/// Returns `Some(message)` when the body parses and `error.message` is
/// non-empty; `None` otherwise (caller falls back to HTTP status text).
pub fn parse_jina_error_body(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let error = v.get("error")?;
    let message = error.get("message")?.as_str()?;
    if message.is_empty() {
        None
    } else {
        Some(message.to_string())
    }
}

/// Check whether a request type is supported by the Jina outbound
/// transformer. Mirrors Go outbound.go:99-108.
///
/// Returns `Ok(())` for `"rerank"` and `"embedding"`, and
/// `Err(InvalidRequest)` for any other type.
pub fn validate_jina_request_type(request_type: &str) -> TransformerResult<()> {
    match request_type {
        "rerank" | "embedding" => Ok(()),
        other => Err(ConduitError::invalid_request(format!(
            "{other} is not supported"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- S04/S08 parse_jina_route ---------------------------------------

    #[test]
    fn parse_route_jina_native_embedding() {
        let r = parse_jina_route("/jina/v1/embeddings");
        assert!(matches!(
            r,
            Some(JinaRoute {
                kind: JinaRouteKind::Embedding,
                jina_native: true
            })
        ));
    }

    #[test]
    fn parse_route_jina_native_rerank() {
        let r = parse_jina_route("/jina/v1/rerank");
        assert!(matches!(
            r,
            Some(JinaRoute {
                kind: JinaRouteKind::Rerank,
                jina_native: true
            })
        ));
    }

    #[test]
    fn parse_route_openai_compatible_rerank() {
        // Go routes.go line 192: /v1/rerank -> handlers.Jina.Rerank
        let r = parse_jina_route("/v1/rerank");
        assert!(matches!(
            r,
            Some(JinaRoute {
                kind: JinaRouteKind::Rerank,
                jina_native: false
            })
        ));
    }

    #[test]
    fn parse_route_openai_compatible_embedding_is_not_native() {
        // Go routes.go line 176: /v1/embeddings -> OpenAI handler. We still
        // recognize it as an embedding route, but jina_native must be false.
        let r = parse_jina_route("/v1/embeddings");
        assert!(matches!(
            r,
            Some(JinaRoute {
                kind: JinaRouteKind::Embedding,
                jina_native: false
            })
        ));
    }

    #[test]
    fn parse_route_unrelated_path_is_none() {
        assert!(parse_jina_route("/v1/chat/completions").is_none());
        assert!(parse_jina_route("/anthropic/v1/messages").is_none());
        assert!(parse_jina_route("/healthz").is_none());
    }

    // ---- S06 validate_jina_input ----------------------------------------

    #[test]
    fn validate_input_valid_string() -> Result<(), ConduitError> {
        validate_jina_input(&json!("The quick brown fox"))
    }

    #[test]
    fn validate_input_empty_string_rejected() {
        match validate_jina_input(&json!("")) {
            Err(err) => {
                assert_eq!(err.message, "input cannot be empty string");
                assert_eq!(err.http_status, 400);
            }
            Ok(()) => panic!("expected Err for empty string input"),
        }
    }

    #[test]
    fn validate_input_whitespace_only_rejected() {
        match validate_jina_input(&json!("   ")) {
            Err(err) => assert_eq!(err.message, "input cannot be empty string"),
            Ok(()) => panic!("expected Err for whitespace-only input"),
        }
    }

    #[test]
    fn validate_input_valid_string_array() -> Result<(), ConduitError> {
        validate_jina_input(&json!(["Hello", "World"]))
    }

    #[test]
    fn validate_input_empty_string_array_rejected() {
        match validate_jina_input(&json!([])) {
            Err(err) => assert_eq!(err.message, "input cannot be empty array"),
            Ok(()) => panic!("expected Err for empty array input"),
        }
    }

    #[test]
    fn validate_input_array_with_empty_element_rejected() {
        match validate_jina_input(&json!(["ok", "  ", "good"])) {
            Err(err) => assert_eq!(err.message, "input[1] cannot be empty string"),
            Ok(()) => panic!("expected Err for array containing whitespace"),
        }
    }

    #[test]
    fn validate_input_valid_int_array() -> Result<(), ConduitError> {
        validate_jina_input(&json!([1, 2, 3]))
    }

    #[test]
    fn validate_input_nested_empty_inner_array_rejected() {
        match validate_jina_input(&json!([[1, 2], []])) {
            Err(err) => assert_eq!(err.message, "input[1] cannot be empty array"),
            Ok(()) => panic!("expected Err for nested empty inner array"),
        }
    }

    #[test]
    fn validate_input_null_rejected() {
        match validate_jina_input(&Value::Null) {
            Err(err) => assert_eq!(err.message, "input cannot be empty string"),
            Ok(()) => panic!("expected Err for null input"),
        }
    }

    // ---- S07 reject_stream ----------------------------------------------

    #[test]
    fn reject_stream_absent_is_ok() -> Result<(), ConduitError> {
        reject_stream(JinaRouteKind::Embedding, None)
    }

    #[test]
    fn reject_stream_false_is_ok() -> Result<(), ConduitError> {
        reject_stream(JinaRouteKind::Rerank, Some(false))
    }

    #[test]
    fn reject_stream_true_is_error() {
        match reject_stream(JinaRouteKind::Embedding, Some(true)) {
            Err(err) => {
                assert_eq!(err.http_status, 400);
                // Go embedding string is byte-exact (plural "do").
                assert_eq!(err.message, "embeddings do not support streaming");
            }
            Ok(()) => panic!("expected Err for stream=true"),
        }
    }

    #[test]
    fn reject_stream_rerank_uses_singular_does() {
        // Go rerank string differs from embedding (singular "does").
        match reject_stream(JinaRouteKind::Rerank, Some(true)) {
            Err(err) => assert_eq!(err.message, "rerank does not support streaming"),
            Ok(()) => panic!("expected Err for stream=true"),
        }
    }

    // ---- S10 build_rerank_response --------------------------------------

    #[test]
    fn build_rerank_response_preserves_casing_no_documents() {
        let hits = vec![
            RerankHit {
                index: 1,
                relevance_score: 0.98,
                document: None,
            },
            RerankHit {
                index: 0,
                relevance_score: 0.42,
                document: None,
            },
        ];
        let resp = build_rerank_response("jina-reranker-v2", "list", &hits, Some((7, 7)));
        // Serialize and assert exact JSON casing mirrors Go model.go tags.
        let body = serde_json::to_value(&resp).unwrap_or(Value::Null);
        assert_eq!(body["model"], "jina-reranker-v2");
        assert_eq!(body["object"], "list");
        assert_eq!(body["results"][0]["index"], 1);
        assert_eq!(body["results"][0]["relevance_score"], 0.98);
        // document omitted when None (Go omitempty).
        assert!(
            body["results"][0]
                .get("document")
                .map(|v| v.is_null())
                .unwrap_or(true)
        );
        assert_eq!(body["usage"]["prompt_tokens"], 7);
        assert_eq!(body["usage"]["total_tokens"], 7);
    }

    #[test]
    fn build_rerank_response_with_documents() {
        let hits = vec![RerankHit {
            index: 0,
            relevance_score: 0.91,
            document: Some("hello world".to_string()),
        }];
        let resp = build_rerank_response("m", "r", &hits, None);
        let body = serde_json::to_value(&resp).unwrap_or(Value::Null);
        assert_eq!(body["results"][0]["document"]["text"], "hello world");
        // usage omitted when None.
        assert!(body.get("usage").map(|v| v.is_null()).unwrap_or(true));
    }

    // ---- S11 validate_jina_batch_size -----------------------------------

    #[test]
    fn batch_size_under_limit_is_ok() -> Result<(), ConduitError> {
        validate_jina_batch_size(1)
    }

    #[test]
    fn batch_size_at_limit_is_ok() -> Result<(), ConduitError> {
        validate_jina_batch_size(JINA_MAX_BATCH_SIZE)
    }

    #[test]
    fn batch_size_over_limit_rejected() {
        match validate_jina_batch_size(JINA_MAX_BATCH_SIZE + 1) {
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert_eq!(
                    err.message,
                    format!(
                        "input batch size {} exceeds maximum of {}",
                        JINA_MAX_BATCH_SIZE + 1,
                        JINA_MAX_BATCH_SIZE
                    )
                );
            }
            Ok(()) => panic!("expected Err for over-limit batch"),
        }
    }

    #[test]
    fn batch_size_zero_is_ok() -> Result<(), ConduitError> {
        // Empty-array rejection is S06's job; S11 only caps the ceiling.
        validate_jina_batch_size(0)
    }

    #[test]
    fn jina_input_count_scalar_string_is_one() {
        assert_eq!(jina_input_count(&json!("hello")), 1);
    }

    #[test]
    fn jina_input_count_string_array_is_len() {
        assert_eq!(jina_input_count(&json!(["a", "b", "c"])), 3);
    }

    #[test]
    fn jina_input_count_int_array_is_len() {
        assert_eq!(jina_input_count(&json!([1, 2, 3, 4])), 4);
    }

    #[test]
    fn jina_input_count_nested_array_is_outer_len() {
        assert_eq!(jina_input_count(&json!([[1, 2], [3]])), 2);
    }

    // ---- S09 resolve_embedding_usage ------------------------------------

    fn usage_with(prompt: u64, total: u64) -> Usage {
        Usage {
            prompt_tokens: prompt,
            total_tokens: total,
            ..Usage::zero()
        }
    }

    #[test]
    fn resolve_usage_some_forwards_prompt_and_total() {
        let u = usage_with(42, 100);
        let resolved = resolve_embedding_usage(Some(&u));
        match resolved {
            Some(r) => {
                assert_eq!(r.prompt_tokens, 42);
                assert_eq!(r.total_tokens, 100);
                // Go's EmbeddingUsage only carries prompt/total; the
                // projection must zero completion_tokens and details.
                assert_eq!(r.completion_tokens, 0);
            }
            None => panic!("expected Some for non-nil usage"),
        }
    }

    #[test]
    fn resolve_usage_none_is_none() {
        assert!(resolve_embedding_usage(None).is_none());
    }

    #[test]
    fn resolve_usage_zero_fields_still_some() {
        // Go forwards a non-nil Usage even when tokens are zero (the nil
        // check is on the struct pointer, not the field values). Mirror
        // that: Some(zero) -> Some(zero).
        let zero = Usage::zero();
        match resolve_embedding_usage(Some(&zero)) {
            Some(r) => assert!(r.is_zero()),
            None => panic!("Go forwards non-nil zero usage; expected Some"),
        }
    }

    #[test]
    fn resolve_usage_drops_completion_tokens() {
        // Go's EmbeddingUsage struct has no completion_tokens field, so any
        // upstream completion count is silently dropped. Mirror that
        // projection.
        let mut u = usage_with(10, 30);
        u.completion_tokens = 99;
        match resolve_embedding_usage(Some(&u)) {
            Some(r) => {
                assert_eq!(r.prompt_tokens, 10);
                assert_eq!(r.total_tokens, 30);
                assert_eq!(r.completion_tokens, 0);
            }
            None => panic!("expected Some"),
        }
    }

    // ---- S12: build_jina_embedding_url ---------------------------------
    // Mirrors Go TestOutboundTransformer_URLBuilding_Embedding (L609-654)
    // and the URL assertions in TestOutboundTransformer_TransformRequest_Embedding
    // (L264, L294).

    #[test]
    fn embedding_url_with_v1_suffix() {
        // Go L616-619: "with /v1 suffix"
        assert_eq!(
            build_jina_embedding_url("https://api.jina.ai/v1", ""),
            "https://api.jina.ai/v1/embeddings"
        );
    }

    #[test]
    fn embedding_url_without_v1_suffix() {
        // Go L620-624: "without /v1 suffix" — version appended by normalizer.
        assert_eq!(
            build_jina_embedding_url("https://api.jina.ai", ""),
            "https://api.jina.ai/v1/embeddings"
        );
    }

    #[test]
    fn embedding_url_with_trailing_slash() {
        // Go L626-629: "with trailing slash" — trim then append version.
        assert_eq!(
            build_jina_embedding_url("https://api.jina.ai/", ""),
            "https://api.jina.ai/v1/embeddings"
        );
    }

    #[test]
    fn embedding_url_with_custom_endpoint_path() {
        // Go outbound.go:58-62 + 246-249: when endpoint_path is set, version
        // normalization is skipped.
        assert_eq!(
            build_jina_embedding_url("https://custom.example.com", "/my-embed"),
            "https://custom.example.com/my-embed"
        );
    }

    // ---- S12: resolve_jina_embedding_task ------------------------------
    // Mirrors Go TestOutboundTransformer_TransformRequest_Embedding subtests
    // "embedding request with explicit task" (L297-322) and
    // "embedding request with empty task defaults to text-matching" (L324-349),
    // plus "valid array input with all task types" (L86-117).

    #[test]
    fn task_empty_defaults_to_text_matching() {
        // Go L324-349: empty task defaults to "text-matching".
        assert_eq!(resolve_jina_embedding_task(""), "text-matching");
    }

    #[test]
    fn task_explicit_value_preserved() {
        // Go L297-322: explicit task "retrieval.query" is passed through.
        assert_eq!(
            resolve_jina_embedding_task("retrieval.query"),
            "retrieval.query"
        );
    }

    #[test]
    fn task_all_recognized_types_preserved() {
        // Go L86-117: all six recognized task types pass through unchanged.
        for task in [
            "text-matching",
            "retrieval.query",
            "retrieval.passage",
            "separation",
            "classification",
            "none",
        ] {
            assert_eq!(resolve_jina_embedding_task(task), task);
        }
    }

    // ---- S12: EmbeddingRequest serialization ---------------------------
    // Mirrors Go outbound.go:193-208 (transformEmbeddingRequest) and the
    // body assertions in TestOutboundTransformer_TransformRequest_Embedding
    // (L269-274, L317-322, L344-349).

    #[test]
    fn embedding_request_serialization_empty_task_gets_default() -> Result<(), serde_json::Error> {
        // Go L324-349: empty task → "text-matching" after defaulting.
        let req = EmbeddingRequest {
            input: json!("Hello world"),
            model: "jina-embeddings-v3".to_string(),
            task: Some(resolve_jina_embedding_task("").to_string()),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let body = serde_json::to_value(&req)?;
        assert_eq!(body["model"], "jina-embeddings-v3");
        assert_eq!(body["input"], "Hello world");
        assert_eq!(body["task"], "text-matching");
        // omitempty fields absent when None.
        assert!(body.get("encoding_format").is_none());
        assert!(body.get("dimensions").is_none());
        assert!(body.get("user").is_none());
        Ok(())
    }

    #[test]
    fn embedding_request_serialization_explicit_task() -> Result<(), serde_json::Error> {
        // Go L297-322: explicit task "retrieval.query" is preserved.
        let req = EmbeddingRequest {
            input: json!("Hello world"),
            model: "jina-embeddings-v3".to_string(),
            task: Some("retrieval.query".to_string()),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let body = serde_json::to_value(&req)?;
        assert_eq!(body["task"], "retrieval.query");
        Ok(())
    }

    #[test]
    fn embedding_request_serialization_string_array_input() -> Result<(), serde_json::Error> {
        // Go L66-84 + L244-274: array input is forwarded as-is.
        let req = EmbeddingRequest {
            input: json!(["Hello", "World"]),
            model: "jina-embeddings-v3".to_string(),
            task: Some("text-matching".to_string()),
            encoding_format: None,
            dimensions: None,
            user: None,
        };
        let body = serde_json::to_value(&req)?;
        assert_eq!(body["input"], json!(["Hello", "World"]));
        Ok(())
    }

    #[test]
    fn embedding_request_serialization_dimensions_and_format() -> Result<(), serde_json::Error> {
        // Go EmbeddingRequest model.go:37-44: dimensions (*int) and
        // encoding_format are forwarded when set.
        let req = EmbeddingRequest {
            input: json!("test"),
            model: "jina-embeddings-v3".to_string(),
            task: None,
            encoding_format: Some("float".to_string()),
            dimensions: Some(768),
            user: Some("user-123".to_string()),
        };
        let body = serde_json::to_value(&req)?;
        assert_eq!(body["encoding_format"], "float");
        assert_eq!(body["dimensions"], 768);
        assert_eq!(body["user"], "user-123");
        // task omitted when None (Go omitempty).
        assert!(body.get("task").is_none());
        Ok(())
    }

    // ---- S12: EmbeddingResponse deserialization ------------------------
    // Mirrors Go TestOutboundTransformer_TransformResponse_Embedding
    // "valid embedding response" (L409-445) and the golden contract case at
    // tests/contracts/llm_cases/jina/embedding_outbound_response.json.

    #[test]
    fn embedding_response_deserialization_valid() -> Result<(), serde_json::Error> {
        // Go L410-424: EmbeddingResponse{Object:"list", Model:"jina-embeddings-v3",
        // Data:[{Object:"embedding", Index:0, Embedding:[0.1,0.2,0.3]}],
        // Usage:{PromptTokens:5, TotalTokens:5}}.
        let body = json!({
            "object": "list",
            "model": "jina-embeddings-v3",
            "data": [
                {
                    "object": "embedding",
                    "index": 0,
                    "embedding": [0.1, 0.2, 0.3]
                }
            ],
            "usage": {
                "prompt_tokens": 5,
                "total_tokens": 5
            }
        });
        let resp: EmbeddingResponse = serde_json::from_value(body)?;
        assert_eq!(resp.object, "list");
        assert_eq!(resp.model, "jina-embeddings-v3");
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].object, "embedding");
        assert_eq!(resp.data[0].index, 0);
        assert_eq!(resp.data[0].embedding, vec![0.1, 0.2, 0.3]);
        assert_eq!(resp.usage.prompt_tokens, 5);
        assert_eq!(resp.usage.total_tokens, 5);
        Ok(())
    }

    #[test]
    fn embedding_response_serialization_round_trip() -> Result<(), serde_json::Error> {
        // Go inbound TransformResponse (embedding_inbound.go:131-158):
        // the inbound response is always serialized with all fields.
        let resp = EmbeddingResponse {
            object: "list".to_string(),
            data: vec![EmbeddingData {
                object: "embedding".to_string(),
                embedding: vec![0.1, 0.2, 0.3],
                index: 0,
            }],
            model: "jina-embeddings-v3".to_string(),
            usage: EmbeddingUsage {
                prompt_tokens: 5,
                total_tokens: 5,
            },
        };
        let body = serde_json::to_value(&resp)?;
        // Usage always present (no omitempty in Go EmbeddingResponse).
        assert_eq!(body["usage"]["prompt_tokens"], 5);
        assert_eq!(body["usage"]["total_tokens"], 5);
        assert_eq!(body["data"][0]["embedding"][0], 0.1);
        assert_eq!(body["data"][0]["index"], 0);
        Ok(())
    }

    #[test]
    fn embedding_response_deserialization_empty_usage_defaults() -> Result<(), serde_json::Error> {
        // Go EmbeddingResponse.Usage is not omitempty; when absent in JSON the
        // Go zero-value is {0, 0}. Rust serde default mirrors this.
        let body = json!({
            "object": "list",
            "model": "m",
            "data": []
        });
        let resp: EmbeddingResponse = serde_json::from_value(body)?;
        assert_eq!(resp.usage.prompt_tokens, 0);
        assert_eq!(resp.usage.total_tokens, 0);
        Ok(())
    }

    // ---- S12: extract_outbound_embedding_usage -------------------------
    // Mirrors Go outbound.go:374 usage gate: only emit usage when
    // PromptTokens > 0 || TotalTokens > 0.

    #[test]
    fn outbound_usage_extracted_when_prompt_positive() {
        assert_eq!(extract_outbound_embedding_usage(5, 0), Some((5, 0)));
    }

    #[test]
    fn outbound_usage_extracted_when_total_positive() {
        assert_eq!(extract_outbound_embedding_usage(0, 5), Some((0, 5)));
    }

    #[test]
    fn outbound_usage_extracted_when_both_positive() {
        // Go L409-445 test case: PromptTokens=5, TotalTokens=5 → usage set.
        assert_eq!(extract_outbound_embedding_usage(5, 5), Some((5, 5)));
    }

    #[test]
    fn outbound_usage_dropped_when_both_zero() {
        // Go outbound.go:374: both 0 → no usage emitted.
        assert_eq!(extract_outbound_embedding_usage(0, 0), None);
    }

    // ---- S12: validate_jina_content_type -------------------------------
    // Mirrors Go TestEmbeddingInboundTransformer_TransformRequest
    // "unsupported content type" (L216-227) and the implicit default
    // (empty content type → application/json) at embedding_inbound.go:35-37.

    #[test]
    fn content_type_empty_defaults_to_ok() -> Result<(), ConduitError> {
        // Go L35-37: empty content type defaults to "application/json".
        validate_jina_content_type("")
    }

    #[test]
    fn content_type_application_json_is_ok() -> Result<(), ConduitError> {
        validate_jina_content_type("application/json")
    }

    #[test]
    fn content_type_with_charset_is_ok() -> Result<(), ConduitError> {
        // Go L39: strings.Contains check, so charset suffix is fine.
        validate_jina_content_type("application/json; charset=utf-8")
    }

    #[test]
    fn content_type_case_insensitive_json_is_ok() -> Result<(), ConduitError> {
        // Go L39: strings.ToLower(contentType) before Contains check.
        validate_jina_content_type("Application/JSON")
    }

    #[test]
    fn content_type_text_plain_rejected() {
        // Go L216-227: "unsupported content type"
        match validate_jina_content_type("text/plain") {
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert!(err.message.contains("unsupported content type"));
                assert!(err.message.contains("text/plain"));
            }
            Ok(()) => panic!("expected Err for text/plain"),
        }
    }

    // ---- S12: validate_jina_model_required -----------------------------
    // Mirrors Go TestEmbeddingInboundTransformer_TransformRequest
    // "missing model" (L119-136).

    #[test]
    fn model_required_present_is_ok() -> Result<(), ConduitError> {
        validate_jina_model_required("jina-embeddings-v3")
    }

    #[test]
    fn model_required_empty_rejected() {
        // Go L119-136: "model is required"
        match validate_jina_model_required("") {
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert!(err.message.contains("model is required"));
            }
            Ok(()) => panic!("expected Err for empty model"),
        }
    }

    // ---- S12: validate_jina_request_type -------------------------------
    // Mirrors Go TestOutboundTransformer_TransformRequest_Embedding
    // "embedding request wrong request type" (L382-398).

    #[test]
    fn request_type_embedding_is_supported() -> Result<(), ConduitError> {
        validate_jina_request_type("embedding")
    }

    #[test]
    fn request_type_rerank_is_supported() -> Result<(), ConduitError> {
        validate_jina_request_type("rerank")
    }

    #[test]
    fn request_type_chat_not_supported() {
        // Go L382-398: chat request type → "<type> is not supported"
        match validate_jina_request_type("chat") {
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert!(err.message.contains("is not supported"));
            }
            Ok(()) => panic!("expected Err for chat request type"),
        }
    }

    // ---- S12: parse_jina_error_body ------------------------------------
    // Mirrors Go TestOutboundTransformer_TransformError_Embedding
    // "jina format error" (L564-574) and "non-json error body" (L576-585).

    #[test]
    fn jina_error_body_json_with_message() {
        // Go L564-574: {"error":{"message":"Invalid model","type":"invalid_request_error"}}
        let body = br#"{"error": {"message": "Invalid model", "type": "invalid_request_error"}}"#;
        match parse_jina_error_body(body) {
            Some(msg) => assert_eq!(msg, "Invalid model"),
            None => panic!("expected Some(message) for valid error JSON"),
        }
    }

    #[test]
    fn jina_error_body_non_json_returns_none() {
        // Go L576-585: "Service unavailable" (non-JSON body) → fall back to
        // HTTP status text.
        let body = b"Service unavailable";
        assert!(parse_jina_error_body(body).is_none());
    }

    #[test]
    fn jina_error_body_empty_message_returns_none() {
        // Go outbound.go:422: jinaError.Error.Message != "" check — empty
        // message falls through to status-text fallback.
        let body = br#"{"error": {"message": "", "type": "x"}}"#;
        assert!(parse_jina_error_body(body).is_none());
    }

    #[test]
    fn jina_error_body_no_error_key_returns_none() {
        let body = br#"{"foo": "bar"}"#;
        assert!(parse_jina_error_body(body).is_none());
    }

    #[test]
    fn jina_error_body_empty_bytes_returns_none() {
        // Go TransformError with empty body → JSON parse fails → status text.
        assert!(parse_jina_error_body(b"").is_none());
    }
}

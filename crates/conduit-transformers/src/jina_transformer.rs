//! Jina inbound transformer — assembles the pure primitives in
//! [`crate::jina`] into a production [`InboundTransformer`] for the Jina
//! rerank + embeddings surface.
//!
//! Mirrors Go `conduit/internal/server/api/jina.go` (which wires
//! `jina.NewRerankInboundTransformer()` / `jina.NewEmbeddingInboundTransformer()`
//! behind a shared `ChatCompletionHandlers`) together with the two Go inbound
//! transformers `conduit/llm/transformer/jina/rerank_inbound.go` and
//! `conduit/llm/transformer/jina/embedding_inbound.go`.
//!
//! Route table (Go `internal/server/routes.go` lines 192 / 196-198):
//! - `POST /v1/rerank`         → rerank  (OpenAI-compatible mount)
//! - `POST /jina/v1/rerank`    → rerank  (Jina-native mount)
//! - `POST /jina/v1/embeddings`→ embedding (Jina-native mount)
//!
//! This module contains no I/O: it reads the already-parsed
//! [`HttpRequest`], runs the pure validators/builders from [`crate::jina`], and
//! produces the unified [`LlmRequest`] / client-facing [`HttpResponse`]. All
//! validation logic lives in `jina.rs` and is imported, never duplicated.

use conduit_core::ConduitError;
use conduit_llm::model::HeaderMap;
use conduit_llm::{
    ApiFormat, EmbeddingRequest as UnifiedEmbeddingRequest, HttpRequest, HttpResponse, LlmRequest,
    LlmRequestPayload, LlmResponse, RequestType, RerankRequest as UnifiedRerankRequest,
    StreamEvent,
};
use serde_json::Value;

use crate::TransformerResult;
use crate::jina::{
    EmbeddingResponse, EmbeddingUsage, JinaRoute, JinaRouteKind, RerankHit, RerankResponse,
    build_rerank_response, jina_input_count, reject_stream, resolve_embedding_usage,
    validate_jina_batch_size, validate_jina_content_type, validate_jina_input,
    validate_jina_model_required,
};
use crate::traits::InboundTransformer;

/// Production inbound transformer for the Jina rerank + embeddings surface.
///
/// A single value carries the route [`JinaRouteKind`] discriminant so one
/// implementation can serve both `/v1|/jina/v1/rerank` and
/// `/jina/v1/embeddings`, dispatching request/response shaping on the kind —
/// mirroring how Go's `JinaHandlers` holds two `ChatCompletionHandlers` built
/// from `NewRerankInboundTransformer` / `NewEmbeddingInboundTransformer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JinaInbound {
    kind: JinaRouteKind,
}

impl JinaInbound {
    /// Construct the rerank inbound transformer (Go
    /// `jina.NewRerankInboundTransformer`).
    pub const fn rerank() -> Self {
        Self {
            kind: JinaRouteKind::Rerank,
        }
    }

    /// Construct the embedding inbound transformer (Go
    /// `jina.NewEmbeddingInboundTransformer`).
    pub const fn embedding() -> Self {
        Self {
            kind: JinaRouteKind::Embedding,
        }
    }

    /// Construct from a classified [`JinaRoute`] (see
    /// [`crate::jina::parse_jina_route`]). Only the route kind is used —
    /// the `jina_native` flag governs *dispatch* to this transformer vs the
    /// OpenAI one and is decided by the caller (bridge/router), not here.
    pub const fn for_route(route: &JinaRoute) -> Self {
        Self { kind: route.kind }
    }

    /// The route kind this transformer serves.
    pub const fn kind(&self) -> JinaRouteKind {
        self.kind
    }
}

impl InboundTransformer for JinaInbound {
    fn name(&self) -> &'static str {
        match self.kind {
            JinaRouteKind::Rerank => "jina/rerank",
            JinaRouteKind::Embedding => "jina/embeddings",
        }
    }

    /// HTTP request → unified [`LlmRequest`].
    ///
    /// Go parity:
    /// `RerankInboundTransformer.TransformRequest` (rerank_inbound.go:25-74) and
    /// `EmbeddingInboundTransformer.TransformRequest` (embedding_inbound.go:22-75).
    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        // Go: `len(httpReq.Body) == 0` → "request body is empty" (both inbound
        // transformers guard this first).
        let body = jina_request_body(&request)?;

        // Stream gate. Go rejects streaming at `TransformStream` time
        // (rerank_inbound.go:185 / embedding_inbound.go:177) with kind-specific
        // byte-exact messages. Here we additionally pre-flight the request body's
        // `stream` flag so a `stream=true` embedding/rerank request fails fast
        // with the same deterministic 400 rather than reaching the (unsupported)
        // streaming path. `reject_stream` selects the plural/singular message by
        // kind.
        reject_stream(self.kind, body.get("stream").and_then(Value::as_bool))?;

        let mut llm_request = match self.kind {
            JinaRouteKind::Rerank => build_rerank_llm_request(body)?,
            JinaRouteKind::Embedding => {
                // Content-type guard is embedding-only in Go
                // (embedding_inbound.go:34-41; the rerank transformer performs no
                // content-type check).
                let content_type = resolve_content_type(&request);
                build_embedding_llm_request(&content_type, body)?
            }
        };

        // Carry HTTP-layer context onto the unified request (mirrors
        // `OpenAiEmbeddingInbound::inbound_request`).
        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    /// Pass-through — the inbound HTTP response envelope is not modified before
    /// [`InboundTransformer::transform_response`] processes the unified
    /// [`LlmResponse`]. Mirrors the Gemini inbound transformer's `inbound_response`
    /// (there is no `httpclient.Response → httpclient.Response` step in the Go
    /// Jina inbound; the real shaping is `TransformResponse(*llm.Response)`).
    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    /// Jina rerank/embeddings never stream. Go's `TransformStream` returns an
    /// error (rerank_inbound.go:181-186 / embedding_inbound.go:173-178); we reuse
    /// [`reject_stream`] to surface the same byte-exact, kind-specific message.
    /// The trailing `Ok` is unreachable (`reject_stream(_, Some(true))` always
    /// errors) but keeps the return type total without `unwrap`/`expect`.
    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        reject_stream(self.kind, Some(true))?;
        Ok(event)
    }

    /// Map a gateway [`ConduitError`] into a Jina-shaped error envelope
    /// `{"error":{"message":..,"type":..}}`.
    ///
    /// Go parity: `RerankInboundTransformer.TransformError`
    /// (rerank_inbound.go:140-178); the embedding transformer delegates to it
    /// (embedding_inbound.go:187-190). The HTTP status is taken from the error's
    /// resolved `http_status`; the public (safe) message is used so internal
    /// details are not leaked.
    fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
        let payload = serde_json::json!({
            "error": {
                "message": error.public_message(),
                "type": error.error_type(),
            }
        });
        let body = serde_json::to_vec(&payload).map_err(|err| {
            ConduitError::internal("failed to marshal jina error response").with_source(err)
        })?;

        let mut headers = HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        Ok(HttpResponse {
            status: error.http_status,
            headers,
            body: Some(body),
            ..HttpResponse::default()
        })
    }

    /// Unified [`LlmResponse`] → client-facing Jina HTTP response.
    ///
    /// Go parity: `RerankInboundTransformer.TransformResponse`
    /// (rerank_inbound.go:77-137) and
    /// `EmbeddingInboundTransformer.TransformResponse`
    /// (embedding_inbound.go:121-171). Dispatches on the route kind and shapes
    /// the body via the [`crate::jina`] response helpers.
    fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
        match self.kind {
            JinaRouteKind::Rerank => transform_rerank_response(response),
            JinaRouteKind::Embedding => transform_embedding_response(response),
        }
    }
}

// ---------------------------------------------------------------------------
// Request-direction builders.
// ---------------------------------------------------------------------------

/// Read the request body as a JSON [`Value`], preferring the already-parsed
/// `json_body` and falling back to raw `body` bytes. Empty body →
/// `InvalidRequest` "request body is empty" (Go `len(httpReq.Body) == 0`).
fn jina_request_body(request: &HttpRequest) -> TransformerResult<Value> {
    if let Some(json_body) = request.json_body.as_ref() {
        return Ok(json_body.clone());
    }
    match request.body.as_deref() {
        Some(bytes) if !bytes.is_empty() => serde_json::from_slice(bytes).map_err(|err| {
            ConduitError::invalid_request("failed to decode request body").with_source(err)
        }),
        _ => Err(ConduitError::invalid_request("request body is empty")),
    }
}

/// Resolve the request content type, mirroring `OpenAiEmbeddingInbound`: the
/// explicit `content_type`, else a case-insensitive `Content-Type` header, else
/// the Go default of `application/json` (embedding_inbound.go:35-37).
fn resolve_content_type(request: &HttpRequest) -> String {
    request
        .content_type
        .as_deref()
        .or_else(|| {
            request.headers.iter().find_map(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    .then_some(value.as_str())
            })
        })
        .unwrap_or("application/json")
        .to_string()
}

/// Build a rerank [`LlmRequest`]. Go parity: rerank_inbound.go:37-73.
fn build_rerank_llm_request(body: Value) -> TransformerResult<LlmRequest> {
    let mut object = match body {
        Value::Object(map) => map,
        _ => {
            return Err(ConduitError::invalid_request(
                "failed to decode rerank request",
            ));
        }
    };

    // Lift the model off the top level (it becomes `LlmRequest.model`, not part
    // of the payload). The `stream` gate already ran in `inbound_request`; drop
    // the flag so it does not leak into the payload's `extra` (Go leaves
    // `Stream = nil` on rerank requests).
    let model = take_top_level_string(&mut object, "model");
    object.remove("stream");

    // Deserialize the remaining object into the unified rerank payload; a
    // type-mismatch (e.g. non-array `documents`) surfaces the Go
    // "failed to decode rerank request" message.
    let payload: UnifiedRerankRequest =
        serde_json::from_value(Value::Object(object)).map_err(|err| {
            ConduitError::invalid_request("failed to decode rerank request").with_source(err)
        })?;

    // Required-field validation, in Go order (model → query → documents).
    validate_jina_model_required(&model)?;
    if payload.query.as_deref().unwrap_or("").is_empty() {
        return Err(ConduitError::invalid_request("query is required"));
    }
    if payload.documents.is_empty() {
        return Err(ConduitError::invalid_request("documents are required"));
    }

    Ok(LlmRequest {
        request_type: RequestType::Rerank,
        api_format: ApiFormat::JinaRerank,
        model: Some(model),
        // Rerank never streams (Go leaves `Stream = nil`).
        stream: false,
        payload: LlmRequestPayload::Rerank(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

/// Build an embedding [`LlmRequest`]. Go parity: embedding_inbound.go:34-74.
fn build_embedding_llm_request(content_type: &str, body: Value) -> TransformerResult<LlmRequest> {
    // Content-type guard (embedding-only in Go).
    validate_jina_content_type(content_type)?;

    let mut object = match body {
        Value::Object(map) => map,
        _ => {
            return Err(ConduitError::invalid_request(
                "failed to decode embedding request",
            ));
        }
    };

    let model = take_top_level_string(&mut object, "model");
    validate_jina_model_required(&model)?;

    // Validate the raw `input` value against the four Go-recognized shapes
    // (string / []string / []int / [][]int) before it is handed to serde, then
    // apply the Rust-side batch-size ceiling.
    let input_value = object.get("input").cloned().unwrap_or(Value::Null);
    validate_jina_input(&input_value)?;
    validate_jina_batch_size(jina_input_count(&input_value))?;

    // The `stream` gate already ran; drop the flag so it does not leak into the
    // payload's `extra` (Go leaves `Stream = nil`). The jina-specific `task`
    // field is intentionally left in place — the unified `EmbeddingRequest`
    // payload has no typed `task`, so it round-trips through the flattened
    // `extra` map.
    object.remove("stream");

    let payload: UnifiedEmbeddingRequest =
        serde_json::from_value(Value::Object(object)).map_err(|err| {
            ConduitError::invalid_request("failed to decode embedding request").with_source(err)
        })?;

    Ok(LlmRequest {
        request_type: RequestType::Embedding,
        api_format: ApiFormat::JinaEmbeddings,
        model: Some(model),
        // Embeddings never stream (Go leaves `Stream = nil`).
        stream: false,
        payload: LlmRequestPayload::Embedding(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

/// Remove `key` from `object` and return it as an owned string when it is a
/// JSON string, else an empty string (so the required-field validators produce
/// the Go "<field> is required" message).
fn take_top_level_string(object: &mut serde_json::Map<String, Value>, key: &str) -> String {
    object
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Response-direction shaping.
// ---------------------------------------------------------------------------

/// Shape a unified rerank [`LlmResponse`] into the Jina rerank HTTP response
/// via [`build_rerank_response`]. Go parity: rerank_inbound.go:77-137.
fn transform_rerank_response(response: LlmResponse) -> TransformerResult<HttpResponse> {
    // Go: `llmResp.Rerank == nil` → error "rerank response is nil".
    let rerank_value = response
        .rerank
        .as_ref()
        .ok_or_else(|| ConduitError::internal("rerank response is nil"))?;

    // The unified `Response.Rerank` sub-body is the Go `llm.RerankResponse`
    // shape `{object, results:[{index, relevance_score, document?}]}`; it
    // deserializes into the jina `RerankResponse` (model/usage default and are
    // overridden below from the parent response).
    let parsed: RerankResponse = serde_json::from_value(rerank_value.clone()).map_err(|err| {
        ConduitError::internal("failed to decode unified rerank response").with_source(err)
    })?;

    let hits: Vec<RerankHit> = parsed
        .results
        .iter()
        .map(|result| RerankHit {
            index: result.index,
            relevance_score: result.relevance_score,
            document: result.document.as_ref().map(|doc| doc.text.clone()),
        })
        .collect();

    // Usage is forwarded only when present (Go: `if llmResp.Usage != nil`).
    let usage = response
        .usage
        .as_ref()
        .map(|u| (u.prompt_tokens as i64, u.total_tokens as i64));

    let jina_response = build_rerank_response(&response.model, &parsed.object, &hits, usage);
    let body = serde_json::to_vec(&jina_response).map_err(|err| {
        ConduitError::internal("failed to marshal rerank response").with_source(err)
    })?;

    // Go rerank response sets only Content-Type (rerank_inbound.go:130-132).
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());

    Ok(HttpResponse {
        status: 200,
        headers,
        body: Some(body),
        ..HttpResponse::default()
    })
}

/// Shape a unified embedding [`LlmResponse`] into the Jina embedding HTTP
/// response via [`EmbeddingResponse`] + [`resolve_embedding_usage`]. Go parity:
/// embedding_inbound.go:121-171.
fn transform_embedding_response(response: LlmResponse) -> TransformerResult<HttpResponse> {
    // Go: nil `llmResp.Embedding` → error "embedding response missing embedding data".
    let embedding_value = response
        .embedding
        .as_ref()
        .ok_or_else(|| ConduitError::internal("embedding response missing embedding data"))?;

    // The unified `Response.Embedding` sub-body is the Go `llm.EmbeddingResponse`
    // shape `{object, data:[{object, embedding, index}]}`; it deserializes into
    // the jina `EmbeddingResponse` (model/usage default and are set below).
    let parsed: EmbeddingResponse =
        serde_json::from_value(embedding_value.clone()).map_err(|err| {
            ConduitError::internal("failed to decode unified embedding response").with_source(err)
        })?;

    // Project the parent usage onto the two-field jina `EmbeddingUsage`. Go
    // sets usage only when `llmResp.Usage != nil`, otherwise leaves the zero
    // value (which still serializes, as the field has no `omitempty`).
    let usage = match resolve_embedding_usage(response.usage.as_ref()) {
        Some(u) => EmbeddingUsage {
            prompt_tokens: u.prompt_tokens as i64,
            total_tokens: u.total_tokens as i64,
        },
        None => EmbeddingUsage::zero(),
    };

    let jina_response = EmbeddingResponse {
        object: parsed.object,
        data: parsed.data,
        model: response.model.clone(),
        usage,
    };
    let body = serde_json::to_vec(&jina_response).map_err(|err| {
        ConduitError::internal("failed to marshal embedding response").with_source(err)
    })?;

    // Go embedding response sets Content-Type + Cache-Control
    // (embedding_inbound.go:166-169).
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Cache-Control".to_string(), "no-cache".to_string());

    Ok(HttpResponse {
        status: 200,
        headers,
        body: Some(body),
        ..HttpResponse::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jina::{JINA_MAX_BATCH_SIZE, parse_jina_route};
    use conduit_llm::Usage;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn http_request(body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/jina/v1/embeddings".to_string(),
            content_type: Some("application/json".to_string()),
            json_body: Some(body),
            ..HttpRequest::default()
        }
    }

    // ---- constructors + name ------------------------------------------------

    #[test]
    fn constructors_map_to_route_kind_and_name() -> TestResult {
        assert_eq!(JinaInbound::rerank().kind(), JinaRouteKind::Rerank);
        assert_eq!(JinaInbound::rerank().name(), "jina/rerank");
        assert_eq!(JinaInbound::embedding().kind(), JinaRouteKind::Embedding);
        assert_eq!(JinaInbound::embedding().name(), "jina/embeddings");
        Ok(())
    }

    #[test]
    fn for_route_uses_parsed_route_kind() -> TestResult {
        let embedding_route = parse_jina_route("/jina/v1/embeddings")
            .ok_or_else(|| ConduitError::internal("route"))?;
        assert_eq!(
            JinaInbound::for_route(&embedding_route).name(),
            "jina/embeddings"
        );

        let rerank_route =
            parse_jina_route("/v1/rerank").ok_or_else(|| ConduitError::internal("route"))?;
        assert_eq!(JinaInbound::for_route(&rerank_route).name(), "jina/rerank");
        Ok(())
    }

    // ---- rerank request → LlmRequest ---------------------------------------

    #[test]
    fn rerank_request_builds_unified_llm_request() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = http_request(json!({
            "model": "jina-reranker-v2",
            "query": "the quick brown fox",
            "documents": ["doc a", "doc b", "doc c"],
            "top_n": 2,
            "return_documents": true
        }));

        let llm = transformer.inbound_request(request)?;
        assert_eq!(llm.request_type, RequestType::Rerank);
        assert_eq!(llm.api_format, ApiFormat::JinaRerank);
        assert_eq!(llm.model.as_deref(), Some("jina-reranker-v2"));
        assert!(!llm.stream);
        match llm.payload {
            LlmRequestPayload::Rerank(rerank) => {
                assert_eq!(rerank.query.as_deref(), Some("the quick brown fox"));
                assert_eq!(rerank.documents.len(), 3);
                assert_eq!(rerank.top_n, Some(2));
                assert!(rerank.return_documents);
            }
            other => {
                return Err(
                    ConduitError::internal(format!("expected rerank payload: {other:?}")).into(),
                );
            }
        }
        Ok(())
    }

    #[test]
    fn rerank_missing_model_is_rejected() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = http_request(json!({"query": "q", "documents": ["a"]}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected model-required error").into()),
            Err(err) => {
                assert_eq!(err.message, "model is required");
                assert_eq!(err.http_status, 400);
                Ok(())
            }
        }
    }

    #[test]
    fn rerank_missing_query_is_rejected() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = http_request(json!({"model": "m", "documents": ["a"]}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected query-required error").into()),
            Err(err) => {
                assert_eq!(err.message, "query is required");
                Ok(())
            }
        }
    }

    #[test]
    fn rerank_empty_documents_is_rejected() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = http_request(json!({"model": "m", "query": "q", "documents": []}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected documents-required error").into()),
            Err(err) => {
                assert_eq!(err.message, "documents are required");
                Ok(())
            }
        }
    }

    // ---- embedding request → LlmRequest ------------------------------------

    #[test]
    fn embedding_request_builds_unified_llm_request() -> TestResult {
        let transformer = JinaInbound::embedding();
        let request = http_request(json!({
            "model": "jina-embeddings-v3",
            "input": ["Hello", "World"],
            "task": "retrieval.query",
            "encoding_format": "float",
            "dimensions": 768,
            "user": "user-123"
        }));

        let llm = transformer.inbound_request(request)?;
        assert_eq!(llm.request_type, RequestType::Embedding);
        assert_eq!(llm.api_format, ApiFormat::JinaEmbeddings);
        assert_eq!(llm.model.as_deref(), Some("jina-embeddings-v3"));
        assert!(!llm.stream);
        match llm.payload {
            LlmRequestPayload::Embedding(embedding) => {
                assert_eq!(embedding.input, Some(json!(["Hello", "World"])));
                assert_eq!(embedding.encoding_format.as_deref(), Some("float"));
                assert_eq!(embedding.dimensions, Some(768));
                assert_eq!(embedding.user.as_deref(), Some("user-123"));
                // The jina-specific `task` field has no typed slot on the unified
                // `EmbeddingRequest`; it round-trips through `extra`.
                assert_eq!(embedding.extra.get("task"), Some(&json!("retrieval.query")));
            }
            other => {
                return Err(ConduitError::internal(format!(
                    "expected embedding payload: {other:?}"
                ))
                .into());
            }
        }
        Ok(())
    }

    #[test]
    fn embedding_scalar_string_input_is_accepted() -> TestResult {
        let transformer = JinaInbound::embedding();
        let request = http_request(json!({"model": "m", "input": "single text"}));
        let llm = transformer.inbound_request(request)?;
        match llm.payload {
            LlmRequestPayload::Embedding(embedding) => {
                assert_eq!(embedding.input, Some(json!("single text")));
                Ok(())
            }
            other => {
                Err(ConduitError::internal(format!("expected embedding payload: {other:?}")).into())
            }
        }
    }

    #[test]
    fn embedding_missing_model_is_rejected() -> TestResult {
        let transformer = JinaInbound::embedding();
        let request = http_request(json!({"input": "hello"}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected model-required error").into()),
            Err(err) => {
                assert_eq!(err.message, "model is required");
                Ok(())
            }
        }
    }

    #[test]
    fn embedding_empty_array_input_is_rejected() -> TestResult {
        let transformer = JinaInbound::embedding();
        let request = http_request(json!({"model": "m", "input": []}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected empty-array error").into()),
            Err(err) => {
                assert_eq!(err.message, "input cannot be empty array");
                Ok(())
            }
        }
    }

    #[test]
    fn embedding_non_json_content_type_is_rejected() -> TestResult {
        let transformer = JinaInbound::embedding();
        let mut request = http_request(json!({"model": "m", "input": "hi"}));
        request.content_type = Some("text/plain".to_string());
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected content-type error").into()),
            Err(err) => {
                assert!(err.message.contains("unsupported content type"));
                assert!(err.message.contains("text/plain"));
                Ok(())
            }
        }
    }

    // ---- batch-size validation ---------------------------------------------

    #[test]
    fn embedding_batch_over_limit_is_rejected() -> TestResult {
        let transformer = JinaInbound::embedding();
        let inputs: Vec<String> = (0..=JINA_MAX_BATCH_SIZE).map(|i| format!("t{i}")).collect();
        assert_eq!(inputs.len(), JINA_MAX_BATCH_SIZE + 1);
        let request = http_request(json!({"model": "m", "input": inputs}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected batch-size error").into()),
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert!(err.message.contains("exceeds maximum"));
                Ok(())
            }
        }
    }

    #[test]
    fn embedding_batch_at_limit_is_accepted() -> TestResult {
        let transformer = JinaInbound::embedding();
        let inputs: Vec<String> = (0..JINA_MAX_BATCH_SIZE).map(|i| format!("t{i}")).collect();
        let request = http_request(json!({"model": "m", "input": inputs}));
        let llm = transformer.inbound_request(request)?;
        assert_eq!(llm.api_format, ApiFormat::JinaEmbeddings);
        Ok(())
    }

    // ---- stream rejected ----------------------------------------------------

    #[test]
    fn embedding_stream_true_is_rejected_plural_message() -> TestResult {
        let transformer = JinaInbound::embedding();
        let request = http_request(json!({"model": "m", "input": "hi", "stream": true}));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected stream rejection").into()),
            Err(err) => {
                assert_eq!(err.http_status, 400);
                assert_eq!(err.message, "embeddings do not support streaming");
                Ok(())
            }
        }
    }

    #[test]
    fn rerank_stream_true_is_rejected_singular_message() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = http_request(json!({
            "model": "m",
            "query": "q",
            "documents": ["a"],
            "stream": true
        }));
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected stream rejection").into()),
            Err(err) => {
                assert_eq!(err.message, "rerank does not support streaming");
                Ok(())
            }
        }
    }

    #[test]
    fn inbound_stream_event_is_rejected() -> TestResult {
        let transformer = JinaInbound::rerank();
        match transformer.inbound_stream_event(StreamEvent::default()) {
            Ok(_) => Err(ConduitError::internal("expected stream-event rejection").into()),
            Err(err) => {
                assert_eq!(err.message, "rerank does not support streaming");
                Ok(())
            }
        }
    }

    // ---- empty body ---------------------------------------------------------

    #[test]
    fn empty_body_is_rejected() -> TestResult {
        let transformer = JinaInbound::rerank();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/rerank".to_string(),
            ..HttpRequest::default()
        };
        match transformer.inbound_request(request) {
            Ok(_) => Err(ConduitError::internal("expected empty-body error").into()),
            Err(err) => {
                assert_eq!(err.message, "request body is empty");
                Ok(())
            }
        }
    }

    // ---- response shaping ---------------------------------------------------

    #[test]
    fn rerank_response_is_shaped_via_helper() -> TestResult {
        let transformer = JinaInbound::rerank();
        let response = LlmResponse {
            model: "jina-reranker-v2".to_string(),
            rerank: Some(json!({
                "object": "list",
                "results": [
                    {"index": 1, "relevance_score": 0.98},
                    {"index": 0, "relevance_score": 0.11, "document": {"text": "doc zero"}}
                ]
            })),
            usage: Some(Usage {
                prompt_tokens: 7,
                total_tokens: 7,
                ..Usage::zero()
            }),
            ..LlmResponse::default()
        };

        let http = transformer.transform_response(response)?;
        assert_eq!(http.status, 200);
        assert_eq!(
            http.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );

        let body = http
            .body
            .ok_or_else(|| ConduitError::internal("missing rerank body"))?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["model"], "jina-reranker-v2");
        assert_eq!(value["object"], "list");
        assert_eq!(value["results"][0]["index"], 1);
        assert_eq!(value["results"][0]["relevance_score"], 0.98);
        assert_eq!(value["results"][1]["document"]["text"], "doc zero");
        assert_eq!(value["usage"]["prompt_tokens"], 7);
        assert_eq!(value["usage"]["total_tokens"], 7);
        Ok(())
    }

    #[test]
    fn rerank_response_missing_sub_body_errors() -> TestResult {
        let transformer = JinaInbound::rerank();
        match transformer.transform_response(LlmResponse::default()) {
            Ok(_) => Err(ConduitError::internal("expected rerank-nil error").into()),
            Err(err) => {
                assert_eq!(err.message, "rerank response is nil");
                Ok(())
            }
        }
    }

    #[test]
    fn embedding_response_is_shaped_via_helper() -> TestResult {
        let transformer = JinaInbound::embedding();
        let response = LlmResponse {
            model: "jina-embeddings-v3".to_string(),
            embedding: Some(json!({
                "object": "list",
                "data": [
                    {"object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3]}
                ]
            })),
            usage: Some(Usage {
                prompt_tokens: 5,
                total_tokens: 5,
                ..Usage::zero()
            }),
            ..LlmResponse::default()
        };

        let http = transformer.transform_response(response)?;
        assert_eq!(http.status, 200);
        assert_eq!(
            http.headers.get("Cache-Control").map(String::as_str),
            Some("no-cache")
        );

        let body = http
            .body
            .ok_or_else(|| ConduitError::internal("missing embedding body"))?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["object"], "list");
        assert_eq!(value["model"], "jina-embeddings-v3");
        assert_eq!(value["data"][0]["object"], "embedding");
        assert_eq!(value["data"][0]["index"], 0);
        assert_eq!(value["data"][0]["embedding"][0], 0.1);
        // Usage is always serialized (Go `EmbeddingResponse.Usage` has no omitempty).
        assert_eq!(value["usage"]["prompt_tokens"], 5);
        assert_eq!(value["usage"]["total_tokens"], 5);
        Ok(())
    }

    #[test]
    fn embedding_response_without_usage_emits_zero() -> TestResult {
        let transformer = JinaInbound::embedding();
        let response = LlmResponse {
            model: "m".to_string(),
            embedding: Some(json!({"object": "list", "data": []})),
            ..LlmResponse::default()
        };
        let http = transformer.transform_response(response)?;
        let body = http
            .body
            .ok_or_else(|| ConduitError::internal("missing embedding body"))?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["usage"]["prompt_tokens"], 0);
        assert_eq!(value["usage"]["total_tokens"], 0);
        Ok(())
    }

    #[test]
    fn embedding_response_missing_sub_body_errors() -> TestResult {
        let transformer = JinaInbound::embedding();
        match transformer.transform_response(LlmResponse::default()) {
            Ok(_) => Err(ConduitError::internal("expected embedding-nil error").into()),
            Err(err) => {
                assert_eq!(err.message, "embedding response missing embedding data");
                Ok(())
            }
        }
    }

    // ---- error envelope -----------------------------------------------------

    #[test]
    fn inbound_error_builds_jina_envelope() -> TestResult {
        let transformer = JinaInbound::rerank();
        let http = transformer.inbound_error(&ConduitError::invalid_request("bad input"))?;
        assert_eq!(http.status, 400);
        let body = http
            .body
            .ok_or_else(|| ConduitError::internal("missing error body"))?;
        let value: Value = serde_json::from_slice(&body)?;
        assert_eq!(value["error"]["message"], "bad input");
        assert_eq!(value["error"]["type"], "invalid_request");
        Ok(())
    }
}

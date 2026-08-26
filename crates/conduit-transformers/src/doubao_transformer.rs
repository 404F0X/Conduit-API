//! Production Doubao / Seedance video-task [`InboundTransformer`].
//!
//! This assembles the request/response surface of the Go
//! `doubao.VideoInboundTransformer`
//! (`conduit/llm/transformer/doubao/video_inbound.go`) — the transformer the Go
//! `DoubaoHandlers` (`conduit/internal/server/api/doubao.go`) wire into
//! - `CreateTask` (`POST /doubao/v3/contents/generations/tasks`) via
//!   `CreateOrchestrator.Process` → `Inbound.TransformRequest`, and
//! - `GetTask`   (`GET  /doubao/v3/contents/generations/tasks/{id}`) via
//!   `VideoService.GetTaskByExternalID` → `Inbound.TransformResponse`.
//! (`DeleteTask` needs no transformer — the Go handler just returns 204.)
//!
//! All heavy lifting lives in the already-ported pure helpers in
//! [`crate::doubao`]; this file only wires them into the [`InboundTransformer`]
//! trait. It does **not** modify `doubao.rs`.
//!
//! ## Method → Go mapping (read [`crate::traits`] for the trait shape)
//!
//! | Rust trait method | Go site |
//! |---|---|
//! | [`DoubaoVideoInbound::inbound_request`] | `TransformRequest` (video_inbound.go:52-113) |
//! | [`DoubaoVideoInbound::transform_response`] | `TransformResponse(ctx, *llm.Response)` (video_inbound.go:143-219) — **the GetTask/CreateTask client-body path** |
//! | [`DoubaoVideoInbound::inbound_stream_event`] | `TransformStream` (video_inbound.go:221-223) — video never streams |
//! | [`DoubaoVideoInbound::inbound_error`] | `TransformError` (video_inbound.go:225-228) — reuses the OpenAI error envelope |
//! | [`DoubaoVideoInbound::inbound_response`] | *supplementary* provider-task-JSON → client-JSON reshaper (see its doc) |
//!
//! ## Create vs get
//!
//! A **single** transformer + a single [`DoubaoVideoInbound::new`] constructor
//! handles both: [`transform_response`](DoubaoVideoInbound::transform_response)
//! branches at runtime on `LlmResponse.object == "video.create"`
//! (video_inbound.go:151), exactly like Go. No enum / second constructor needed.
//!
//! ## API format
//!
//! Uses [`ApiFormat::SeedanceVideo`] (`"seedance/video"`), the Rust parity of Go
//! `llm.APIFormatSeedanceVideo` (video_inbound.go:110). No separate
//! `doubao/video` format exists or is required — Seedance *is* the Doubao
//! video-task format.

use conduit_core::{ConduitError, ErrorKind, openai_error_json};
use conduit_llm::{
    ApiFormat, HttpRequest, HttpResponse, LlmRequest, LlmRequestPayload, LlmResponse, RequestType,
    StreamEvent, VideoRequest,
};
use serde::Deserialize;
use serde_json::Value;

use crate::TransformerResult;
use crate::doubao::{
    DoubaoTaskView, SeedanceCreateRequest, SeedanceGetUsage, SeedanceUnifiedResponse,
    map_task_status, normalize_seedance_create_request, shape_seedance_create_response,
    shape_seedance_get_response, to_unified_external_id, validate_size,
};
use crate::traits::InboundTransformer;

/// Inbound transformer for the native Doubao/Seedance video-task surface
/// (`/doubao/v3/contents/generations/tasks[/{id}]`). Mirrors Go
/// `doubao.VideoInboundTransformer`.
#[derive(Debug, Default, Clone, Copy)]
pub struct DoubaoVideoInbound;

impl DoubaoVideoInbound {
    /// Construct the transformer. Parity with Go
    /// `doubao.NewVideoInboundTransformer()` (video_inbound.go:25-27).
    pub const fn new() -> Self {
        Self
    }
}

/// Typed view of the fields of Go `llm.VideoResponse` (`conduit/llm/video.go:69-101`)
/// that `TransformResponse` reads. The unified Rust [`LlmResponse::video`] is an
/// opaque `serde_json::Value` (the dedicated video sub-response type is not yet
/// ported), so we decode just this subset here.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
struct VideoResponseView {
    id: String,
    status: String,
    model: String,
    video_url: String,
    // Duration is a string in `llm.VideoResponse` to preserve fractional
    // provider values (video.go:86-88); `shape_seedance_get_response` parses +
    // rounds it back to an int64.
    duration: Option<String>,
    ratio: String,
    resolution: String,
    fps: Option<i64>,
    seed: Option<i64>,
    created_at: i64,
    completed_at: i64,
}

impl InboundTransformer for DoubaoVideoInbound {
    fn name(&self) -> &'static str {
        "doubao/video"
    }

    /// Convert the Doubao/Seedance create-task body into a unified
    /// [`LlmRequest`]. Mirrors Go `TransformRequest` (video_inbound.go:52-113):
    /// require JSON, decode `seedanceCreateRequest`, reject empty model/content,
    /// convert `duration` int → string, and forward every other field.
    ///
    /// Because the unified [`VideoRequest`] only models `prompt`/`image`/
    /// `duration`/`size` typed fields, the Seedance-specific fields (`content`,
    /// `ratio`, `resolution`, `frames`, `seed`, `generate_audio`,
    /// `camera_fixed`, `watermark`, `draft`, `service_tier`,
    /// `execution_expires_after`) ride losslessly on [`VideoRequest::extra`],
    /// so serializing the payload reproduces the Go `llm.VideoRequest` JSON.
    ///
    /// **Superset note (reported to the coordinator):** Go's *inbound* does not
    /// read `size`; the `size → ratio/resolution` mapping is applied by the Go
    /// *outbound* (`video_outbound.go:37-48`) under the precedence
    /// `ratio=="" && resolution=="" && size!=""`. To honor the requested
    /// [`validate_size`] use, we apply that **exact same rule + error string**
    /// one stage earlier: for a `size`-only body we map to ratio/resolution and
    /// reject an unmappable `size`; for the native ratio/resolution body the
    /// branch is dormant and behavior is byte-identical to Go.
    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request_json_body(&request)?;

        // Content-type guard — Go video_inbound.go:61-68 requires JSON.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        // Typed decode + validation (model non-empty, content non-empty) and
        // duration int → string, all via the ported doubao.rs helper.
        let create_req: SeedanceCreateRequest =
            serde_json::from_value(body.clone()).map_err(|err| {
                ConduitError::invalid_request("failed to decode seedance video request")
                    .with_source(err)
            })?;
        let unified = normalize_seedance_create_request(create_req)?;

        // Rebuild the body the unified `VideoRequest` deserializer will see:
        // drop `model` (carried on `LlmRequest.model`), re-stamp `duration` as
        // the Go string form, optionally map `size` → ratio/resolution.
        let mut object = match body {
            Value::Object(map) => map,
            _ => {
                return Err(ConduitError::invalid_request(
                    "doubao create-task body must be a JSON object",
                ));
            }
        };
        object.remove("model");
        object.remove("duration");
        if let Some(duration) = unified.duration.as_ref() {
            object.insert("duration".to_string(), Value::String(duration.clone()));
        }

        // Go outbound size precedence (video_outbound.go:41-48), applied here.
        let ratio_set = object
            .get("ratio")
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let resolution_set = object
            .get("resolution")
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !ratio_set && !resolution_set {
            let size = object
                .get("size")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let Some(size) = size {
                // Rejects an unmappable size with Go's exact error message
                // (video_outbound.go:44).
                let aspect = validate_size(&size)?;
                object.insert("ratio".to_string(), Value::String(aspect.ratio.to_string()));
                object.insert(
                    "resolution".to_string(),
                    Value::String(aspect.resolution.to_string()),
                );
            }
        }

        let payload: VideoRequest =
            serde_json::from_value(Value::Object(object)).map_err(|err| {
                ConduitError::invalid_request("failed to decode seedance video request")
                    .with_source(err)
            })?;

        let mut llm_request = LlmRequest {
            request_type: RequestType::Video,
            api_format: ApiFormat::SeedanceVideo,
            model: Some(unified.model),
            // Videos never stream (Go sets Stream = lo.ToPtr(false),
            // video_inbound.go:107).
            stream: false,
            payload: LlmRequestPayload::Video(payload),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        };

        // Carry request headers/metadata forward, mirroring `OpenAiVideoInbound`.
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

    /// **Supplementary** provider-task-JSON → client-JSON reshaper.
    ///
    /// This is NOT the Go `TransformResponse` (that is
    /// [`transform_response`](Self::transform_response), which takes an
    /// [`LlmResponse`]). The Rust trait's `inbound_response` takes an
    /// [`HttpResponse`], so this method reshapes a *provider* Seedance task
    /// response body into the compact Doubao client task JSON via
    /// [`DoubaoTaskView`], normalizing the provider `status` token through
    /// [`map_task_status`] and surfacing the unified external id (via
    /// [`to_unified_external_id`]) on `metadata["external_id"]`.
    ///
    /// **Fidelity note (reported):** [`DoubaoTaskView`] is a *subset* of the Go
    /// `seedanceGetResponseInbound` shape — it omits `seed`/`ratio`/
    /// `resolution`/`duration`/`framespersecond`. The GetTask/CreateTask client
    /// body MUST therefore be produced by
    /// [`transform_response`](Self::transform_response) (full Go parity); this
    /// method is only for the light task-status-view use case.
    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        let body_value = response_json_body(&response)?;
        let mut view: DoubaoTaskView = serde_json::from_value(body_value).map_err(|err| {
            ConduitError::invalid_request("failed to decode doubao task response").with_source(err)
        })?;

        // Normalize the provider status token to the unified
        // queued/running/succeeded/failed/canceled vocabulary.
        if let Some(status) = view.status.as_deref() {
            view.status = Some(map_task_status(status).as_provider_str().to_string());
        }

        let external_id = to_unified_external_id(&view);
        let body = serde_json::to_vec(&view).map_err(|err| {
            ConduitError::internal(format!("failed to marshal doubao task response: {err}"))
        })?;

        let mut out = HttpResponse {
            status: 200,
            body: Some(body),
            ..HttpResponse::default()
        };
        out.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        out.headers
            .insert("Cache-Control".to_string(), "no-cache".to_string());
        if !external_id.is_empty() {
            out.metadata
                .insert("external_id".to_string(), Value::String(external_id));
        }
        Ok(out)
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        // Go video_inbound.go:221-223: video requests do not support streaming.
        Err(ConduitError::invalid_request(
            "video request does not support streaming",
        ))
    }

    fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
        // Go video_inbound.go:225-228 reuses the OpenAI inbound error envelope.
        let body = serde_json::to_vec(&openai_error_json(error)).map_err(|err| {
            ConduitError::internal(format!("failed to marshal doubao error response: {err}"))
        })?;
        let mut out = HttpResponse {
            status: error.http_status,
            body: Some(body),
            ..HttpResponse::default()
        };
        out.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        Ok(out)
    }

    /// Shape a unified [`LlmResponse`] into the Doubao/Seedance client task JSON.
    ///
    /// This is the Rust parity of Go `VideoInboundTransformer.TransformResponse`
    /// (`video_inbound.go:143-219`) — the method the Go `GetTask`/`CreateTask`
    /// handlers call (`doubao.go:124`). The Go trait method
    /// `TransformResponse(ctx, *llm.Response) (*httpclient.Response, error)` maps
    /// to this Rust trait method (see [`crate::traits`] `transform_response`),
    /// which we override with the Seedance-specific shaping:
    ///
    /// - nil video sub-response → `ErrInvalidResponse` (video_inbound.go:144-146);
    /// - `object == "video.create"` → `{ "id": ... }` create ack
    ///   (video_inbound.go:151-165) via [`shape_seedance_create_response`];
    /// - otherwise → the full get-task body (id/model/status/content.video_url/
    ///   usage/created_at/updated_at/seed/resolution/ratio/duration/
    ///   framespersecond) via [`shape_seedance_get_response`].
    fn transform_response(&self, response: LlmResponse) -> TransformerResult<HttpResponse> {
        // Go: `if llmResp == nil || llmResp.Video == nil { ErrInvalidResponse }`.
        let video_value = response.video.clone().ok_or_else(|| {
            ConduitError::new(ErrorKind::InvalidResponse, "video response is nil")
        })?;
        let view: VideoResponseView = serde_json::from_value(video_value).map_err(|err| {
            ConduitError::new(
                ErrorKind::InvalidResponse,
                "failed to decode video response",
            )
            .with_source(err)
        })?;

        let usage = response.usage.as_ref().map(|u| SeedanceGetUsage {
            completion_tokens: u.completion_tokens as i64,
            total_tokens: u.total_tokens as i64,
        });

        let unified = SeedanceUnifiedResponse {
            object: response.object.clone(),
            id: view.id,
            status: view.status,
            model: view.model,
            created_at: view.created_at,
            completed_at: view.completed_at,
            ratio: view.ratio,
            resolution: view.resolution,
            fps: view.fps,
            seed: view.seed,
            duration: view.duration,
            video_url: view.video_url,
            usage,
        };

        let body = if response.object == "video.create" {
            shape_seedance_create_response(&unified)?
        } else {
            // Go uses `time.Now().Unix()` only when CompletedAt == 0
            // (video_inbound.go:172); `shape_seedance_get_response` takes it as
            // a parameter so the pure helper stays deterministic.
            let now_unix = chrono::Utc::now().timestamp();
            shape_seedance_get_response(&unified, now_unix)?
        };

        let mut out = HttpResponse {
            status: 200,
            body: Some(body),
            ..HttpResponse::default()
        };
        // Go stamps both headers on the create and get responses
        // (video_inbound.go:160-163, 214-217).
        out.headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        out.headers
            .insert("Cache-Control".to_string(), "no-cache".to_string());
        Ok(out)
    }
}

/// Read the inbound request body as JSON, preferring the pre-parsed
/// `json_body`. Mirrors the empty-body guard of Go `TransformRequest`
/// (video_inbound.go:57-59) and the handler-level check (doubao.go:78-81).
fn request_json_body(request: &HttpRequest) -> TransformerResult<Value> {
    if let Some(json_body) = &request.json_body {
        return Ok(json_body.clone());
    }
    match request.body.as_deref() {
        Some(bytes) if !bytes.is_empty() => serde_json::from_slice(bytes).map_err(|err| {
            ConduitError::invalid_request("failed to decode seedance video request")
                .with_source(err)
        }),
        _ => Err(ConduitError::invalid_request("request body is empty")),
    }
}

/// Read a response body as JSON, preferring the pre-parsed `json_body`.
fn response_json_body(response: &HttpResponse) -> TransformerResult<Value> {
    if let Some(json_body) = &response.json_body {
        return Ok(json_body.clone());
    }
    match response.body.as_deref() {
        Some(bytes) if !bytes.is_empty() => serde_json::from_slice(bytes).map_err(|err| {
            ConduitError::invalid_request("doubao task response body must be valid JSON")
                .with_source(err)
        }),
        _ => Err(ConduitError::invalid_request(
            "doubao task response body is required",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::Usage;
    use serde_json::json;

    fn json_request(body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/doubao/v3/contents/generations/tasks".to_string(),
            json_body: Some(body),
            ..HttpRequest::default()
        }
    }

    // ---- inbound_request: native create body (content/ratio/resolution) ----

    #[test]
    fn inbound_request_parses_native_create_body() -> Result<(), Box<dyn std::error::Error>> {
        let request = json_request(json!({
            "model": "seedance-1-0-pro-250528",
            "content": [
                {"type": "text", "text": "a cat walking"},
                {"type": "image_url", "image_url": {"url": "https://e/i.png"}, "role": "first_frame"}
            ],
            "duration": 5,
            "ratio": "16:9",
            "resolution": "1080p",
            "frames": 120,
            "seed": 42,
            "generate_audio": true,
            "service_tier": "default"
        }));

        let llm = DoubaoVideoInbound::new().inbound_request(request)?;

        assert_eq!(llm.request_type, RequestType::Video);
        assert_eq!(llm.api_format, ApiFormat::SeedanceVideo);
        assert_eq!(llm.model.as_deref(), Some("seedance-1-0-pro-250528"));
        assert!(!llm.stream);

        let LlmRequestPayload::Video(video) = llm.payload else {
            return Err("expected Video payload".into());
        };
        // Go converts duration int64 → string (video_inbound.go:98-101).
        assert_eq!(video.duration.as_deref(), Some("5"));
        // model is carried on LlmRequest.model, NOT duplicated onto the payload.
        assert!(video.extra.get("model").is_none());
        // Seedance-specific fields ride VideoRequest.extra losslessly.
        assert_eq!(
            video.extra.get("ratio").and_then(Value::as_str),
            Some("16:9")
        );
        assert_eq!(
            video.extra.get("resolution").and_then(Value::as_str),
            Some("1080p")
        );
        assert_eq!(
            video
                .extra
                .get("content")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(video.extra.get("frames").and_then(Value::as_i64), Some(120));
        assert_eq!(video.extra.get("seed").and_then(Value::as_i64), Some(42));
        assert_eq!(
            video.extra.get("generate_audio").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            video.extra.get("service_tier").and_then(Value::as_str),
            Some("default")
        );
        Ok(())
    }

    // ---- inbound_request: OpenAI-style size → ratio/resolution mapping ----

    #[test]
    fn inbound_request_maps_size_to_ratio_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let request = json_request(json!({
            "model": "seedance-1-0",
            "content": [{"type": "text", "text": "a cat"}],
            "size": "1280x720"
        }));

        let llm = DoubaoVideoInbound::new().inbound_request(request)?;
        let LlmRequestPayload::Video(video) = llm.payload else {
            return Err("expected Video payload".into());
        };
        // size preserved on the typed field; ratio/resolution derived via
        // validate_size (Go's size table).
        assert_eq!(video.size.as_deref(), Some("1280x720"));
        assert_eq!(
            video.extra.get("ratio").and_then(Value::as_str),
            Some("16:9")
        );
        assert_eq!(
            video.extra.get("resolution").and_then(Value::as_str),
            Some("720p")
        );
        Ok(())
    }

    #[test]
    fn inbound_request_keeps_explicit_ratio_over_size() -> Result<(), Box<dyn std::error::Error>> {
        // When ratio/resolution are present the size branch is dormant (Go
        // outbound precedence): the explicit ratio wins and is not overwritten.
        let request = json_request(json!({
            "model": "seedance-1-0",
            "content": [{"type": "text", "text": "a cat"}],
            "ratio": "9:16",
            "resolution": "1080p",
            "size": "1280x720"
        }));

        let llm = DoubaoVideoInbound::new().inbound_request(request)?;
        let LlmRequestPayload::Video(video) = llm.payload else {
            return Err("expected Video payload".into());
        };
        assert_eq!(
            video.extra.get("ratio").and_then(Value::as_str),
            Some("9:16")
        );
        assert_eq!(
            video.extra.get("resolution").and_then(Value::as_str),
            Some("1080p")
        );
        Ok(())
    }

    // ---- inbound_request: rejections ----

    #[test]
    fn inbound_request_rejects_invalid_size() -> Result<(), Box<dyn std::error::Error>> {
        let request = json_request(json!({
            "model": "seedance-1-0",
            "content": [{"type": "text", "text": "x"}],
            "size": "1024x1024"
        }));

        match DoubaoVideoInbound::new().inbound_request(request) {
            Ok(_) => Err("expected invalid-size rejection".into()),
            Err(err) => {
                // Go's exact wrapping message (video_outbound.go:44).
                assert_eq!(
                    err.message,
                    r#"size "1024x1024" cannot be mapped to ratio/resolution, please set ratio and resolution"#
                );
                Ok(())
            }
        }
    }

    #[test]
    fn inbound_request_rejects_missing_model() -> Result<(), Box<dyn std::error::Error>> {
        let request = json_request(json!({
            "content": [{"type": "text", "text": "x"}]
        }));
        match DoubaoVideoInbound::new().inbound_request(request) {
            Ok(_) => Err("expected model-required error".into()),
            Err(err) => {
                assert_eq!(err.message, "model is required");
                Ok(())
            }
        }
    }

    #[test]
    fn inbound_request_rejects_empty_content() -> Result<(), Box<dyn std::error::Error>> {
        let request = json_request(json!({
            "model": "seedance-1-0",
            "content": []
        }));
        match DoubaoVideoInbound::new().inbound_request(request) {
            Ok(_) => Err("expected content-required error".into()),
            Err(err) => {
                assert_eq!(err.message, "content is required");
                Ok(())
            }
        }
    }

    #[test]
    fn inbound_request_rejects_non_json_content_type() -> Result<(), Box<dyn std::error::Error>> {
        let mut request = json_request(json!({
            "model": "m",
            "content": [{"type": "text", "text": "x"}]
        }));
        request.content_type = Some("text/plain".to_string());
        match DoubaoVideoInbound::new().inbound_request(request) {
            Ok(_) => Err("expected unsupported-content-type error".into()),
            Err(err) => {
                assert_eq!(err.message, "unsupported content type: text/plain");
                Ok(())
            }
        }
    }

    // ---- transform_response: get-task client body (Go TransformResponse) ----

    #[test]
    fn transform_response_shapes_get_task_json() -> Result<(), Box<dyn std::error::Error>> {
        let response = LlmResponse {
            object: "video.task".to_string(),
            video: Some(json!({
                "id": "t1",
                "status": "succeeded",
                "model": "seedance-1",
                "video_url": "https://e/v.mp4",
                "duration": "3.6",
                "ratio": "16:9",
                "resolution": "1080p",
                "fps": 30,
                "seed": 7,
                "created_at": 1_700_000_000_i64,
                "completed_at": 1_700_000_100_i64
            })),
            usage: Some(Usage {
                completion_tokens: 100,
                total_tokens: 120,
                ..Usage::default()
            }),
            ..LlmResponse::default()
        };

        let http = DoubaoVideoInbound::new().transform_response(response)?;
        assert_eq!(http.status, 200);
        assert_eq!(
            http.headers.get("Content-Type").map(String::as_str),
            Some("application/json")
        );
        let body = http.body.ok_or("missing body")?;
        let v: Value = serde_json::from_slice(&body)?;

        assert_eq!(v["id"], "t1");
        assert_eq!(v["status"], "succeeded");
        assert_eq!(v["model"], "seedance-1");
        assert_eq!(v["content"]["video_url"], "https://e/v.mp4");
        // "3.6" → math.Round → 4 (video_inbound.go:180-183).
        assert_eq!(v["duration"], 4);
        assert_eq!(v["ratio"], "16:9");
        assert_eq!(v["resolution"], "1080p");
        // Go wire tag is the single word "framespersecond".
        assert_eq!(v["framespersecond"], 30);
        assert_eq!(v["seed"], 7);
        assert_eq!(v["created_at"], 1_700_000_000_i64);
        // CompletedAt != 0 → updated_at == completed_at (video_inbound.go:172).
        assert_eq!(v["updated_at"], 1_700_000_100_i64);
        assert_eq!(v["usage"]["completion_tokens"], 100);
        assert_eq!(v["usage"]["total_tokens"], 120);
        Ok(())
    }

    // ---- transform_response: create ack ({id} only) ----

    #[test]
    fn transform_response_shapes_create_ack() -> Result<(), Box<dyn std::error::Error>> {
        let response = LlmResponse {
            object: "video.create".to_string(),
            video: Some(json!({ "id": "c1723abc" })),
            ..LlmResponse::default()
        };
        let http = DoubaoVideoInbound::new().transform_response(response)?;
        let body = http.body.ok_or("missing body")?;
        let v: Value = serde_json::from_slice(&body)?;
        assert_eq!(v["id"], "c1723abc");
        // Create ack is exactly {"id": "..."} (video_inbound.go:151-160).
        assert_eq!(v.as_object().map(serde_json::Map::len), Some(1));
        Ok(())
    }

    #[test]
    fn transform_response_rejects_missing_video() -> Result<(), Box<dyn std::error::Error>> {
        let response = LlmResponse {
            object: "video.task".to_string(),
            video: None,
            ..LlmResponse::default()
        };
        match DoubaoVideoInbound::new().transform_response(response) {
            Ok(_) => Err("expected nil-video rejection".into()),
            Err(err) => {
                assert_eq!(err.message, "video response is nil");
                Ok(())
            }
        }
    }

    // ---- inbound_response: DoubaoTaskView reshape + status mapping ----

    #[test]
    fn inbound_response_reshapes_task_with_status_mapping() -> Result<(), Box<dyn std::error::Error>>
    {
        let provider = HttpResponse {
            status: 200,
            json_body: Some(json!({
                "id": "t1",
                "model": "seedance-1",
                "status": "running",
                "content": {"video_url": "https://e/v.mp4"},
                "usage": {"completion_tokens": 5, "total_tokens": 6},
                "created_at": 1_700_000_000_i64,
                "updated_at": 1_700_000_100_i64
            })),
            ..HttpResponse::default()
        };

        let http = DoubaoVideoInbound::new().inbound_response(provider)?;
        assert_eq!(http.status, 200);
        // to_unified_external_id surfaced on metadata.
        assert_eq!(
            http.metadata.get("external_id").and_then(Value::as_str),
            Some("t1")
        );
        let body = http.body.ok_or("missing body")?;
        let v: Value = serde_json::from_slice(&body)?;
        assert_eq!(v["id"], "t1");
        // map_task_status("running") → Processing → "running" (idempotent).
        assert_eq!(v["status"], "running");
        assert_eq!(v["content"]["video_url"], "https://e/v.mp4");
        assert_eq!(v["usage"]["completion_tokens"], 5);
        assert_eq!(v["usage"]["total_tokens"], 6);
        assert_eq!(v["created_at"], 1_700_000_000_i64);
        assert_eq!(v["updated_at"], 1_700_000_100_i64);
        Ok(())
    }

    #[test]
    fn inbound_response_normalizes_non_canonical_status() -> Result<(), Box<dyn std::error::Error>>
    {
        // A non-canonical provider status normalizes through the biz mapping:
        // "completed" is not a Seedance terminal → Processing → "running"
        // (doubao.rs map_task_status parity with biz/video.go default arm).
        let provider = HttpResponse {
            status: 200,
            json_body: Some(json!({ "id": "t9", "status": "completed" })),
            ..HttpResponse::default()
        };
        let http = DoubaoVideoInbound::new().inbound_response(provider)?;
        let body = http.body.ok_or("missing body")?;
        let v: Value = serde_json::from_slice(&body)?;
        assert_eq!(v["status"], "running");
        Ok(())
    }

    // ---- streaming unsupported ----

    #[test]
    fn inbound_stream_event_rejects_streaming() -> Result<(), Box<dyn std::error::Error>> {
        match DoubaoVideoInbound::new().inbound_stream_event(StreamEvent::default()) {
            Ok(_) => Err("expected streaming rejection".into()),
            Err(err) => {
                assert_eq!(err.message, "video request does not support streaming");
                Ok(())
            }
        }
    }

    // ---- inbound_error: OpenAI-shaped envelope ----

    #[test]
    fn inbound_error_uses_openai_envelope() -> Result<(), Box<dyn std::error::Error>> {
        let http =
            DoubaoVideoInbound::new().inbound_error(&ConduitError::invalid_request("bad body"))?;
        assert_eq!(http.status, 400);
        let body = http.body.ok_or("missing body")?;
        let v: Value = serde_json::from_slice(&body)?;
        assert_eq!(v["error"]["message"], "bad body");
        assert_eq!(v["error"]["type"], "invalid_request");
        Ok(())
    }
}

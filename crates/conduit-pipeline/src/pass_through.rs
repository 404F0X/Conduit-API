//! Pipeline-side pass-through seams (RUST-P8-003, S04-S11).
//!
//! Ports the **pipeline-layer concerns** of Go's pass-through machinery in
//! `conduit/internal/server/orchestrator/pass_through.go`. The Go file lives in
//! the orchestrator package but every concrete pass-through "applier" it builds
//! (`applyPassThroughRequestBody` `:76`, `applyPassThroughResponse` `:228`,
//! `applyPassThroughStream` `:349`, `captureRawProviderResponse` `:216`,
//! `captureRawProviderStream` `:252`, `applyUserAgentPassThrough` `:171`) is a
//! `pipeline.Middleware` constructed against the same 9-hook
//! [`crate::middleware::PipelineMiddleware`] surface this crate exposes. So the
//! split of concerns the task asks us to encode is:
//!
//! | Task item | Pipeline-side deliverable | Owner of the live wiring |
//! |-----------|---------------------------|--------------------------|
//! | **S04** request-body short-circuit | pure helpers [`pass_through_body_supported`] / [`pass_through_body_needs_model_patch`] / [`merge_pass_through_request_body`] / [`pass_through_stream_aligned`]; decision shape [`PassThroughDecision`] | orchestrator's `applyPassThroughRequestBody` middleware (Go `:76`) |
//! | **S05** response/stream passthrough of raw bytes | decision flags on [`PassThroughDecision`] (response/stream independent of request — S08) | orchestrator's `applyPassThroughResponse` (`:228`) / `applyPassThroughStream` (`:349`) |
//! | **S06** persistence still happens | trait seam [`PassThroughRecorder`] (request/execution persistence + usage capture live in the orchestrator recorder) | orchestrator recorder |
//! | **S07** `PassThroughApplied` marker | trait seam method [`PassThroughRecorder::record_pass_through_applied`] | orchestrator writes the `RequestExecution` row |
//! | **S08** three independent toggles | [`PassThroughDecision`] has separate `request_body_enabled` / `response_enabled` / `stream_enabled` flags — NOT one bool | decision assembled by orchestrator from `ChannelSettings.PassThroughBody` + `systemService.PassThrough` (Go `:48-58`) |
//! | **S09** RequestExecution + UsageLog always recorded; usage fallback | trait seam [`PassThroughRecorder::record_usage_fallback`] for the unparseable-usage branch | orchestrator recorder (Go fallback in `recorder.go`) |
//! | **S10** stream error frames still go through `FormatStreamError` | callback alias [`FormatStreamErrorFn`] + [`format_stream_error_with`] wrapper the stream middleware must call on error frames | the format body itself lives in `conduit-transformers`/http layer |
//! | **S11** header whitelist + channel overrides | pure helper [`filter_pass_through_headers`]; the **whitelist table** is owned by the channel-override layer (no Go package-level constant exists in `pass_through.go` — only `User-Agent` is touched at `:199-209`) | channel-override / orchestrator |
//!
//! Pure helpers here are unit-tested against the Go golden cases
//! (`passThroughBodySupported`, `passThroughBodyNeedsModelPatch`,
//! `passThroughStreamAligned` from `pass_through.go`). The trait seams are
//! documented with their Go owner so the orchestrator porter knows where to
//! wire them; the pipeline crate stays free of the orchestrator dependency.

use std::collections::BTreeMap;

use conduit_core::ConduitError;
use conduit_llm::ApiFormat;

// ---------------------------------------------------------------------------
// S08 — pass-through decision (three independent toggles).
// ---------------------------------------------------------------------------

/// Effective per-direction pass-through flags for one attempt. Mirrors the
/// three Go middlewares' independent `isPassThroughEnabled` checks:
///
/// - **request body** — consulted by `applyPassThroughRequestBody`
///   (`pass_through.go:80`) and `applyUserAgentPassThrough` (`:171`).
/// - **response** — consulted by `applyPassThroughResponse` (`:230`) and
///   `captureRawProviderResponse` (`:218`).
/// - **stream** — consulted by `applyPassThroughStream` (`:351`) and
///   `captureRawProviderStream` (`:254`).
///
/// In Go each middleware re-evaluates `isPassThroughEnabled` because the
/// effective flag is the channel-level `PassThroughBody` when set, otherwise
/// the global `systemService.PassThrough(ctx)` (`pass_through.go:45-58`).
/// The decision is identical across the three for body, but the task spec
/// (S08) requires the toggles to be **independent** so a future channel may
/// disable response pass-through while keeping request-body pass-through on.
/// The orchestrator assembles this struct; the pipeline consumes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PassThroughDecision {
    /// S04 — replace the outbound request body with the raw inbound body
    /// (skipping the outbound transform's body shaping). Go `:110-111`.
    pub request_body_enabled: bool,
    /// S05 — return the raw provider response bytes to the client without
    /// re-serializing through the inbound response transformer. Go `:244`.
    pub response_enabled: bool,
    /// S05 — return the raw provider stream events to the client without
    /// re-serializing each event through the inbound stream transformer.
    /// Go `:379`.
    pub stream_enabled: bool,
}

impl PassThroughDecision {
    /// All three toggles off — pass-through fully disabled. The default;
    /// pipelines that never configure pass-through observe this and every
    /// helper below short-circuits to "no-op".
    pub const fn disabled() -> Self {
        Self {
            request_body_enabled: false,
            response_enabled: false,
            stream_enabled: false,
        }
    }

    /// All three toggles on — the historical single-flag behavior
    /// (`PassThroughBody=true` applied to request, response and stream).
    pub const fn all_enabled() -> Self {
        Self {
            request_body_enabled: true,
            response_enabled: true,
            stream_enabled: true,
        }
    }

    /// Whether ANY direction is enabled — cheapest pre-check before consulting
    /// the per-direction predicates.
    pub const fn any_enabled(self) -> bool {
        self.request_body_enabled || self.response_enabled || self.stream_enabled
    }
}

// ---------------------------------------------------------------------------
// S04 — pure predicates mirroring Go pass_through.go.
// ---------------------------------------------------------------------------

/// Mirrors Go `passThroughBodySupported` (`pass_through.go:138-149`): reports
/// whether the raw inbound body can safely replace the outbound request body.
/// Multipart formats are excluded because the outbound transformer rebuilds
/// the multipart payload with a new boundary in `Content-Type`, so replaying
/// the inbound bytes would mismatch the header, and form fields cannot be
/// patched via `sjson`. Enum mapping (Go → Rust `ApiFormat`):
///
/// | Go constant | Rust variant |
/// |-------------|--------------|
/// | `APIFormatOpenAITranscription` | `OpenAiAudioTranscriptions` |
/// | `APIFormatOpenAITranslation`   | `OpenAiAudioTranslations`   |
/// | `APIFormatOpenAIImageEdit`     | `OpenAiImageEdit`           |
/// | `APIFormatOpenAIImageVariation`| `OpenAiImageVariation`      |
pub const fn pass_through_body_supported(fmt: ApiFormat) -> bool {
    !matches!(
        fmt,
        ApiFormat::OpenAiAudioTranscriptions
            | ApiFormat::OpenAiAudioTranslations
            | ApiFormat::OpenAiImageEdit
            | ApiFormat::OpenAiImageVariation
    )
}

/// Mirrors Go `passThroughBodyNeedsModelPatch` (`pass_through.go:151-167`):
/// reports whether the request body encodes the selected model in a top-level
/// `model` JSON field that must be rewritten with the mapped
/// `LlmRequest.Model` so pass-through does not bypass model mapping. The list
/// is verbatim from the Go switch (excluding the multipart formats that
/// [`pass_through_body_supported`] already rejects). Enum mapping:
///
/// | Go constant | Rust variant |
/// |-------------|--------------|
/// | `APIFormatOpenAIChatCompletion`  | `OpenAiChatCompletions`  |
/// | `APIFormatOpenAIResponse`        | `OpenAiResponses`        |
/// | `APIFormatOpenAIResponseCompact` | `OpenAiResponsesCompact` |
/// | `APIFormatOpenAIEmbedding`       | `OpenAiEmbeddings`       |
/// | `APIFormatJinaEmbedding`         | `JinaEmbeddings`         |
/// | `APIFormatJinaRerank`            | `JinaRerank`             |
/// | `APIFormatAnthropicMessage`      | `AnthropicMessages`      |
/// | `APIFormatOpenAISpeech`          | `OpenAiAudioSpeech`      |
pub const fn pass_through_body_needs_model_patch(fmt: ApiFormat) -> bool {
    matches!(
        fmt,
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

/// Mirrors Go `mergePassThroughRequestBody` (`pass_through.go:117-134`): clone
/// the raw inbound body, and for formats that encode the model in the body
/// ([`pass_through_body_needs_model_patch`]) overwrite the top-level `model`
/// field with the mapped `model` so pass-through does not bypass model
/// mapping. Empty `model` keeps the body unchanged (Go `:124-126`). A body
/// that fails JSON parsing is returned as `Err` so the caller (the
/// orchestrator middleware) can fall back to the transformed outbound body —
/// mirroring Go's "log Warn and keep outbound body" (`pass_through.go:101-108`).
///
/// **Concrete behavior:**
/// - Input `raw_body` is treated as immutable; a cloned `Vec<u8>` is returned.
/// - If the format does not need a model patch → returns the clone unchanged.
/// - If `model` is empty → returns the clone unchanged.
/// - Otherwise parses the body as a JSON object and sets `obj["model"] = model`.
///   A non-object (array, primitive) body yields `Err`; the orchestrator
///   middleware treats the error as "keep outbound body" (Go parity).
pub fn merge_pass_through_request_body(
    raw_body: &[u8],
    api_format: ApiFormat,
    model: &str,
) -> Result<Vec<u8>, ConduitError> {
    let mut body = raw_body.to_vec();
    if !pass_through_body_needs_model_patch(api_format) {
        return Ok(body);
    }
    if model.is_empty() {
        return Ok(body);
    }
    let mut value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|err| ConduitError::internal(format!("parse pass-through body as JSON: {err}")))?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| ConduitError::internal("pass-through body is not a JSON object"))?;
    obj.insert(
        "model".to_string(),
        serde_json::Value::String(model.to_string()),
    );
    body = serde_json::to_vec(&value)
        .map_err(|err| ConduitError::internal(format!("serialize pass-through body: {err}")))?;
    Ok(body)
}

/// Mirrors Go `passThroughStreamAligned` (`pass_through.go:64-69`): the
/// original inbound `stream` flag (Go `state.OriginalRequestStream`) and the
/// effective flag actually sent upstream (Go `llmReq.Stream`) must match —
/// `nil` counts as `false` (Go `originalEnabled := originalStream != nil &&
/// *originalStream`). Mismatch disables pass-through so the pipeline does not
/// hand a stream to a client that asked for a non-stream response (or vice
/// versa).
pub const fn pass_through_stream_aligned(
    original_stream: Option<bool>,
    effective_stream: Option<bool>,
) -> bool {
    let original_enabled = matches!(original_stream, Some(true));
    let effective_enabled = matches!(effective_stream, Some(true));
    original_enabled == effective_enabled
}

// ---------------------------------------------------------------------------
// S11 — header pass-through filter.
// ---------------------------------------------------------------------------

/// Filter the outbound request headers so only the **whitelisted** names and
/// the channel-override-allowed names pass through to the upstream provider.
///
/// This is the pipeline-side half of S11: a pure filter function. The
/// **whitelist table itself is NOT synthesized here** — Go's `pass_through.go`
/// only ever sets `User-Agent` (at `:199-209`); there is no package-level
/// constant listing a header whitelist in either `llm/pipeline/` or
/// `internal/server/orchestrator/`. The actual allow-list the production
/// orchestrator uses is assembled from channel-override operations
/// (`ChannelOverrideTemplate.OverrideOperations` / `HeaderOverrideOperations`,
/// owned by the orchestrator + transform layer). Callers pass the merged
/// allow-list in; this function applies it.
///
/// Header name comparison is **case-insensitive** (HTTP header semantics, RFC
/// 7230 §3.2). The whitelist is pre-lowercased by the caller; this function
/// lowercases each outbound header name on the fly.
///
/// `channel_allowed_lower` is the per-channel override list (already
/// lowercased); `global_whitelist_lower` is the system-wide constant list
/// (already lowercased). A header passes when its lowercased name appears in
/// either set.
pub fn filter_pass_through_headers(
    outbound_headers: &BTreeMap<String, String>,
    global_whitelist_lower: &[&str],
    channel_allowed_lower: &[&str],
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in outbound_headers {
        let lower = name.to_ascii_lowercase();
        let allowed = global_whitelist_lower.contains(&lower.as_str())
            || channel_allowed_lower.contains(&lower.as_str());
        if allowed {
            out.insert(name.clone(), value.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// S06 / S07 / S09 — recorder trait seam (orchestrator-owned).
// ---------------------------------------------------------------------------

/// Recorder seam the orchestrator implements to keep
/// request-execution persistence, usage capture and the
/// `PassThroughApplied` marker flowing while the pipeline runs pass-through.
///
/// **归属判定 (ownership):** all four methods are **orchestrator-owned**.
/// The Go implementation lives in `internal/server/orchestrator/recorder.go`
/// and the pass-through marker (`state.PassThroughApplied`,
/// `pass_through.go:111`) is read by the recorder off the outbound
/// transformer state. The pipeline calls these at the right moments; the
/// orchestrator decides what to persist. Keeping the trait in the pipeline
/// crate lets the pass-through middleware (also pipeline-layer) drive the
/// recorder without depending on the orchestrator crate.
pub trait PassThroughRecorder: Send + Sync {
    /// S06 — record the request/execution persistence row for this attempt.
    /// Called after the outbound request is finalized (Go recorder's
    /// `persistRequestExecution`, `outbound.go`).
    fn record_execution(&self, attempt: &PassThroughAttempt) -> Result<(), ConduitError>;

    /// S07 — flip the `PassThroughApplied` marker on the current
    /// `RequestExecution` row. Called immediately after the pass-through
    /// middleware replaces the request body (Go `:111`). Idempotent.
    fn record_pass_through_applied(&self, attempt: &PassThroughAttempt)
    -> Result<(), ConduitError>;

    /// S09 — record a UsageLog row. When the raw pass-through body cannot be
    /// parsed for usage (Go recorder falls back to zero-usage or a configured
    /// fallback), the orchestrator records the fallback instead of skipping
    /// the row entirely. Called once per successful attempt.
    fn record_usage(&self, attempt: &PassThroughAttempt) -> Result<(), ConduitError>;

    /// S09 — record the usage **fallback** row specifically (Go fallback path
    /// when the response/stream bytes cannot be parsed into a `Usage`). The
    /// pipeline calls this when its own parse attempt fails; the orchestrator
    /// owns the fallback shape.
    fn record_usage_fallback(
        &self,
        attempt: &PassThroughAttempt,
        reason: &str,
    ) -> Result<(), ConduitError>;
}

/// Snapshot of one pass-through attempt the recorder consumes. Built by the
/// pass-through middleware at attempt boundary; the orchestrator-side
/// implementation translates it into the persistent row shape.
#[derive(Clone, Debug, Default)]
pub struct PassThroughAttempt {
    /// 1-based attempt sequence (mirrors `AttemptRecord::sequence`).
    pub sequence: u32,
    /// Channel id attempted.
    pub channel_id: String,
    /// Effective decision for this attempt (which directions pass-through'd).
    pub decision: PassThroughDecision,
    /// Raw inbound request body bytes captured for the recorder (request
    /// pass-through path only; `None` for response/stream-only pass-through).
    pub raw_request_body: Option<Vec<u8>>,
    /// Raw provider response bytes captured (response pass-through path only).
    pub raw_response_body: Option<Vec<u8>>,
    /// Number of events in the raw provider stream (stream pass-through path
    /// only). The events themselves are owned by the stream consumer; this
    /// count is what the recorder needs for its UsageLog fallback.
    pub raw_stream_event_count: Option<usize>,
}

// ---------------------------------------------------------------------------
// S10 — stream-error format callback.
// ---------------------------------------------------------------------------

/// Callback shape for [`format_stream_error_with`]. Real implementation lives
/// in the transformer/http layer (`FormatStreamError`); the pipeline takes it
/// as a closure so the pass-through stream middleware can format error
/// frames without depending on the transformer crate.
pub type FormatStreamErrorFn = std::sync::Arc<dyn Fn(&ConduitError) -> Vec<u8> + Send + Sync>;

/// Wrap an error frame's payload through the format function, mirroring the
/// Go contract that pass-through streams **must** still send error frames
/// through `FormatStreamError` (so the client receives a properly shaped
/// error event, not raw upstream bytes). Returns the formatted payload the
/// middleware should emit on the wire.
///
/// If `format` is `None` the error's plain text message is returned — the
/// middleware should treat this as "no formatter wired" and skip the frame
/// (the orchestrator always wires one in production).
pub fn format_stream_error_with(
    format: Option<&FormatStreamErrorFn>,
    err: &ConduitError,
) -> Vec<u8> {
    match format {
        Some(formatter) => formatter(err),
        None => err.message.as_bytes().to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Tests — mirror Go pass-through helpers' golden cases.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- S08 decision shape -------------------------------------------------

    #[test]
    fn disabled_decision_is_all_off_and_short_circuits() {
        let d = PassThroughDecision::disabled();
        assert!(!d.request_body_enabled);
        assert!(!d.response_enabled);
        assert!(!d.stream_enabled);
        assert!(!d.any_enabled());
    }

    #[test]
    fn all_enabled_decision_is_legacy_single_flag_shape() {
        let d = PassThroughDecision::all_enabled();
        assert!(d.request_body_enabled);
        assert!(d.response_enabled);
        assert!(d.stream_enabled);
        assert!(d.any_enabled());
    }

    #[test]
    fn s08_toggles_are_independent() {
        // S08 — pass-through is NOT one boolean. A future channel may enable
        // request-body pass-through while leaving response/stream on the
        // transformed path. The decision shape must express that.
        let d = PassThroughDecision {
            request_body_enabled: true,
            response_enabled: false,
            stream_enabled: false,
        };
        assert!(d.any_enabled());
        assert!(d.request_body_enabled);
        assert!(!d.response_enabled);
        assert!(!d.stream_enabled);
    }

    // ---- S04 body supported / needs model patch (verbatim Go tables) --------

    #[test]
    fn s04_pass_through_body_supported_mirrors_go_exclusion_list() {
        // Go passThroughBodySupported (pass_through.go:138-149) excludes the
        // four multipart formats and allows everything else. Assert each
        // excluded variant explicitly and spot-check a few allowed ones.
        let excluded = [
            ApiFormat::OpenAiAudioTranscriptions,
            ApiFormat::OpenAiAudioTranslations,
            ApiFormat::OpenAiImageEdit,
            ApiFormat::OpenAiImageVariation,
        ];
        for fmt in excluded {
            assert!(
                !pass_through_body_supported(fmt),
                "{fmt:?} must be excluded (multipart, Go parity)"
            );
        }
        let allowed = [
            ApiFormat::OpenAiChatCompletions,
            ApiFormat::AnthropicMessages,
            ApiFormat::JinaRerank,
            ApiFormat::GeminiContents,
        ];
        for fmt in allowed {
            assert!(
                pass_through_body_supported(fmt),
                "{fmt:?} must be allowed (non-multipart)"
            );
        }
    }

    #[test]
    fn s04_pass_through_body_needs_model_patch_mirrors_go_table() {
        // Go passThroughBodyNeedsModelPatch (pass_through.go:151-167). Assert
        // each variant on the Go list returns true and a representative few
        // off-list variants return false.
        let needs_patch = [
            ApiFormat::OpenAiChatCompletions,
            ApiFormat::OpenAiResponses,
            ApiFormat::OpenAiResponsesCompact,
            ApiFormat::OpenAiEmbeddings,
            ApiFormat::JinaEmbeddings,
            ApiFormat::JinaRerank,
            ApiFormat::AnthropicMessages,
            ApiFormat::OpenAiAudioSpeech,
        ];
        for fmt in needs_patch {
            assert!(
                pass_through_body_needs_model_patch(fmt),
                "{fmt:?} must need a model patch (Go parity)"
            );
        }
        let no_patch = [
            ApiFormat::GeminiContents,
            ApiFormat::OpenAiImageGeneration,
            ApiFormat::AiSdkText,
        ];
        for fmt in no_patch {
            assert!(
                !pass_through_body_needs_model_patch(fmt),
                "{fmt:?} must NOT need a model patch"
            );
        }
    }

    // ---- S04 body merge -----------------------------------------------------

    fn body_as_json(bytes: &[u8]) -> serde_json::Value {
        // Test-only helper; bytes are constructed by the test to be valid JSON.
        // Panics are acceptable here — the test owns the input.
        match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(err) => panic!("test body must parse as JSON: {err}"),
        }
    }

    #[test]
    fn s04_merge_patches_model_for_chat_completions() -> Result<(), Box<dyn std::error::Error>> {
        let raw = br#"{"model":"old","messages":[]}"#;
        let merged =
            merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "mapped-model")?;
        assert_eq!(body_as_json(&merged)["model"], json!("mapped-model"));
        assert_eq!(
            body_as_json(&merged)["messages"],
            json!([]),
            "non-model fields are preserved"
        );
        Ok(())
    }

    #[test]
    fn s04_merge_is_a_noop_for_formats_without_model_field()
    -> Result<(), Box<dyn std::error::Error>> {
        // Gemini contents is not on the needs-patch list -> body returned
        // unchanged regardless of model.
        let raw = br#"{"contents":[]}"#;
        let merged = merge_pass_through_request_body(raw, ApiFormat::GeminiContents, "ignored")?;
        assert_eq!(merged, raw);
        Ok(())
    }

    #[test]
    fn s04_merge_is_a_noop_when_model_is_empty() -> Result<(), Box<dyn std::error::Error>> {
        // Go :124-126 — empty model keeps the body unchanged even for formats
        // that would otherwise need a patch.
        let raw = br#"{"model":"keep-me"}"#;
        let merged = merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "")?;
        assert_eq!(merged, raw);
        Ok(())
    }

    #[test]
    fn s04_merge_returns_err_for_non_object_body() {
        // A non-JSON-object body cannot be patched; the orchestrator
        // middleware treats the error as "keep outbound body" (Go :101-108).
        let raw = br#"true"#;
        let err = match merge_pass_through_request_body(raw, ApiFormat::OpenAiChatCompletions, "x")
        {
            Ok(_) => panic!("non-object body must fail"),
            Err(err) => err,
        };
        assert_eq!(err.kind, conduit_core::ErrorKind::Internal);
    }

    // ---- S04 stream-aligned (Go :64-69) -------------------------------------

    #[test]
    fn s04_pass_through_stream_aligned_mirrors_go_predicate() {
        // None == false on both sides (Go :65-66).
        assert!(pass_through_stream_aligned(None, None));
        assert!(pass_through_stream_aligned(None, Some(false)));
        assert!(pass_through_stream_aligned(Some(false), None));
        assert!(pass_through_stream_aligned(Some(false), Some(false)));
        assert!(pass_through_stream_aligned(Some(true), Some(true)));
        // Mismatched sides disable pass-through.
        assert!(!pass_through_stream_aligned(Some(true), Some(false)));
        assert!(!pass_through_stream_aligned(Some(true), None));
        assert!(!pass_through_stream_aligned(None, Some(true)));
        assert!(!pass_through_stream_aligned(Some(false), Some(true)));
    }

    // ---- S11 header filter --------------------------------------------------

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn s11_filter_keeps_whitelisted_and_channel_overrides_case_insensitively() {
        let outbound = headers(&[
            ("Authorization", "Bearer x"),
            ("Content-Type", "application/json"),
            ("X-Custom", "v"),
            ("User-Agent", "client/1.0"),
        ]);
        let global = ["authorization", "content-type"];
        let channel = ["x-custom"];
        let filtered = filter_pass_through_headers(&outbound, &global, &channel);
        assert!(filtered.contains_key("Authorization"));
        assert!(filtered.contains_key("Content-Type"));
        assert!(filtered.contains_key("X-Custom"));
        assert!(
            !filtered.contains_key("User-Agent"),
            "User-Agent is not in either allow-list -> dropped"
        );
    }

    #[test]
    fn s11_filter_with_empty_lists_drops_everything() {
        let outbound = headers(&[("Authorization", "Bearer x")]);
        let filtered = filter_pass_through_headers(&outbound, &[], &[]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn s11_filter_case_insensitive_match() {
        // Whitelist lowercased; outbound mixed-case — must still match.
        let outbound = headers(&[("AUTHORIZATION", "Bearer x")]);
        let filtered = filter_pass_through_headers(&outbound, &["authorization"], &[]);
        assert!(filtered.contains_key("AUTHORIZATION"));
    }

    // ---- S10 stream-error format wrapper ------------------------------------

    #[test]
    fn s10_format_stream_error_uses_formatter_when_provided() {
        let formatter: FormatStreamErrorFn =
            std::sync::Arc::new(|_err: &ConduitError| b"formatted-error-frame".to_vec());
        let err = ConduitError::upstream("boom");
        let out = format_stream_error_with(Some(&formatter), &err);
        assert_eq!(out, b"formatted-error-frame");
    }

    #[test]
    fn s10_format_stream_error_falls_back_to_message_when_no_formatter() {
        // No formatter wired — the wrapper falls back to the error's plain
        // text message. The orchestrator always wires one in production.
        let err = ConduitError::upstream("raw message");
        let out = format_stream_error_with(None, &err);
        assert_eq!(out, b"raw message");
    }

    // ---- S06/S07/S09 recorder seam — surface only --------------------------

    /// Concrete recorder is owned by the orchestrator; the pipeline only
    /// defines the trait. We verify the trait is object-safe (can be made a
    /// trait object) so the orchestrator can hand in a `dyn` reference.
    struct DummyRecorder;
    impl PassThroughRecorder for DummyRecorder {
        fn record_execution(&self, _attempt: &PassThroughAttempt) -> Result<(), ConduitError> {
            Ok(())
        }
        fn record_pass_through_applied(
            &self,
            _attempt: &PassThroughAttempt,
        ) -> Result<(), ConduitError> {
            Ok(())
        }
        fn record_usage(&self, _attempt: &PassThroughAttempt) -> Result<(), ConduitError> {
            Ok(())
        }
        fn record_usage_fallback(
            &self,
            _attempt: &PassThroughAttempt,
            _reason: &str,
        ) -> Result<(), ConduitError> {
            Ok(())
        }
    }

    #[test]
    fn s06_s07_s09_recorder_trait_is_object_safe() -> Result<(), ConduitError> {
        let recorder: Box<dyn PassThroughRecorder> = Box::new(DummyRecorder);
        let attempt = PassThroughAttempt {
            sequence: 1,
            channel_id: "ch-1".to_string(),
            decision: PassThroughDecision::all_enabled(),
            raw_request_body: Some(br#"{"x":1}"#.to_vec()),
            raw_response_body: None,
            raw_stream_event_count: None,
        };
        // All four methods callable through the trait object.
        recorder.record_execution(&attempt)?;
        recorder.record_pass_through_applied(&attempt)?;
        recorder.record_usage(&attempt)?;
        recorder.record_usage_fallback(&attempt, "unparseable usage")?;
        Ok(())
    }
}

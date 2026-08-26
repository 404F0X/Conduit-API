//! Pipeline middleware wrapping the `ensure_usage` transform (P-40).
//!
//! Go parity: `stream.EnsureUsage()` (`orchestrator/orchestrator.go:78`,
//! `llm/pipeline/stream/usage.go`). For a streaming request it forces
//! `stream_options.include_usage = true` so the upstream provider returns a
//! usage block in the final chunk — without which streaming requests carry no
//! token counts and cannot be billed. For non-streaming requests it is a no-op.
//!
//! The core transform lives as the pure `crate::orchestrator::ensure_usage`
//! function (also unit-tested there); this is the thin middleware that runs it
//! on the inbound hook, matching Go's global middleware position.

use conduit_llm::LlmRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Forces `include_usage` on streaming requests so the provider returns usage.
pub struct EnsureUsageMiddleware;

impl PipelineMiddleware for EnsureUsageMiddleware {
    fn name(&self) -> &'static str {
        "ensure-usage"
    }

    fn on_inbound_llm_request(
        &self,
        _ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        // No-op for non-streaming; forces `stream_options.include_usage = true`
        // for streaming (Go `stream.EnsureUsage`).
        let _ = crate::orchestrator::ensure_usage(&mut request);
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ApiFormat, ChatRequest, LlmRequestPayload, RequestType};
    use serde_json::Value;

    fn chat_request(stream: bool) -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-4o".to_string()),
            stream,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn include_usage(request: &LlmRequest) -> Option<bool> {
        let LlmRequestPayload::Chat(chat) = &request.payload else {
            return None;
        };
        match chat.stream_options.as_ref()? {
            Value::Object(map) => map.get("include_usage").and_then(Value::as_bool),
            _ => None,
        }
    }

    #[test]
    fn streaming_request_gets_include_usage_forced() -> Result<(), Box<dyn std::error::Error>> {
        let mw = EnsureUsageMiddleware;
        let mut ctx = PipelineContext::new();
        let out = mw.on_inbound_llm_request(&mut ctx, chat_request(true))?;
        assert_eq!(
            include_usage(&out),
            Some(true),
            "streaming request must have include_usage forced true"
        );
        Ok(())
    }

    #[test]
    fn non_streaming_request_is_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let mw = EnsureUsageMiddleware;
        let mut ctx = PipelineContext::new();
        let out = mw.on_inbound_llm_request(&mut ctx, chat_request(false))?;
        // Non-streaming: no stream_options injected.
        assert_eq!(
            include_usage(&out),
            None,
            "non-streaming request must not be modified"
        );
        Ok(())
    }
}

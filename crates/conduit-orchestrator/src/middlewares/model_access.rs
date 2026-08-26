//! Pipeline middleware that checks whether the requested model is allowed
//! by the API key's active profile. Go parity: `checkApiKeyModelAccess`
//! (orchestrator/model_access.go:14-54).

use conduit_core::ConduitError;
use conduit_llm::LlmRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Middleware that checks if the requested model is in the API key's
/// allowed model list. Reads `api_key_allowed_models` from context
/// metadata (comma-separated model IDs set by the HTTP auth middleware).
/// Empty or absent = all models allowed.
pub struct CheckModelAccessMiddleware;

impl PipelineMiddleware for CheckModelAccessMiddleware {
    fn name(&self) -> &'static str {
        "check-api-key-model-access"
    }

    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        let model = match &request.model {
            Some(m) if !m.is_empty() => m.as_str(),
            _ => {
                return Err(ConduitError::new(
                    conduit_core::ErrorKind::InvalidRequest,
                    "request model is empty",
                ));
            }
        };

        // Read allowed models from context (set by HTTP auth middleware).
        let allowed_models = ctx
            .metadata
            .get("api_key_allowed_models")
            .map(|s| s.as_str())
            .unwrap_or("");

        // Empty = no restriction (Go: len(profile.ModelIDs) == 0 → pass).
        if allowed_models.is_empty() {
            return Ok(request);
        }

        // Check if model is in the comma-separated whitelist.
        let allowed = allowed_models.split(',').any(|m| m.trim() == model);
        if !allowed {
            return Err(ConduitError::new(
                conduit_core::ErrorKind::InvalidRequest,
                format!("model access denied: {model}"),
            ));
        }

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatRequest, LlmRequestPayload};

    fn chat_request(model: &str) -> LlmRequest {
        LlmRequest {
            request_type: conduit_llm::RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some(model.to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    #[test]
    fn no_restriction_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CheckModelAccessMiddleware;
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4o"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }

    #[test]
    fn allowed_model_passes() -> Result<(), Box<dyn std::error::Error>> {
        let mw = CheckModelAccessMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "api_key_allowed_models".to_string(),
            "gpt-4o,claude-3-opus".to_string(),
        );
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4o"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }

    #[test]
    fn denied_model_rejected() {
        let mw = CheckModelAccessMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "api_key_allowed_models".to_string(),
            "gpt-4o,claude-3-opus".to_string(),
        );
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("llama-3"));
        assert!(result.is_err());
    }

    #[test]
    fn empty_model_rejected() {
        let mw = CheckModelAccessMiddleware;
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request(""));
        assert!(result.is_err());
    }
}

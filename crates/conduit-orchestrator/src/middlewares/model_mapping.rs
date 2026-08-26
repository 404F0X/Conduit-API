//! Pipeline middleware that applies API key profile model mapping — renames
//! the client's requested model to a different upstream model based on the
//! API key's active profile configuration.
//!
//! Go parity: `applyModelMapping` (orchestrator/model_mapper.go:18-160).

use conduit_llm::LlmRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Middleware that remaps the requested model name based on API key profile
/// model mapping configuration. Reads `api_key_model_mapping` from context
/// metadata (JSON: `{"source_model": "target_model", ...}`).
pub struct ModelMappingMiddleware;

impl PipelineMiddleware for ModelMappingMiddleware {
    fn name(&self) -> &'static str {
        "apply-model-mapping"
    }

    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        let model = match &request.model {
            Some(m) if !m.is_empty() => m.clone(),
            _ => return Ok(request),
        };

        // Read model mapping from context (JSON object: {"gpt-4": "gpt-4o", ...}).
        let mapping_json = match ctx.metadata.get("api_key_model_mapping") {
            Some(json) if !json.is_empty() => json.clone(),
            _ => return Ok(request),
        };

        let mapping: serde_json::Map<String, serde_json::Value> =
            match serde_json::from_str(&mapping_json) {
                Ok(m) => m,
                Err(_) => return Ok(request),
            };

        if mapping.is_empty() {
            return Ok(request);
        }

        // Check for exact match first (Go: direct map lookup).
        if let Some(target) = mapping.get(&model).and_then(|v| v.as_str()) {
            // Store original model for logging/tracking.
            ctx.metadata.insert("original_model".to_string(), model);
            request.model = Some(target.to_string());
            return Ok(request);
        }

        // Check for regex pattern matches (Go: xregexp.MatchString).
        for (pattern, target) in &mapping {
            if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
                // Glob-like pattern: convert to regex.
                let regex_pattern = glob_to_regex(pattern);
                if let Ok(re) = regex::Regex::new(&regex_pattern)
                    && re.is_match(&model)
                    && let Some(target_str) = target.as_str()
                {
                    ctx.metadata.insert("original_model".to_string(), model);
                    request.model = Some(target_str.to_string());
                    return Ok(request);
                }
            }
        }

        Ok(request)
    }
}

/// Convert a simple glob pattern to a regex (Go: xregexp).
fn glob_to_regex(glob: &str) -> String {
    let mut regex = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '(' | ')' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
            _ => regex.push(c),
        }
    }
    regex.push('$');
    regex
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
    fn exact_match_remaps_model() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelMappingMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "api_key_model_mapping".to_string(),
            r#"{"gpt-4": "gpt-4o-mini"}"#.to_string(),
        );
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(
            ctx.metadata.get("original_model").map(|s| s.as_str()),
            Some("gpt-4")
        );
        Ok(())
    }

    #[test]
    fn no_mapping_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelMappingMiddleware;
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4"));
        Ok(())
    }

    #[test]
    fn unmatched_model_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelMappingMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "api_key_model_mapping".to_string(),
            r#"{"gpt-3.5-turbo": "gpt-4o-mini"}"#.to_string(),
        );
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-4"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4"));
        Ok(())
    }

    #[test]
    fn glob_pattern_match() -> Result<(), Box<dyn std::error::Error>> {
        let mw = ModelMappingMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "api_key_model_mapping".to_string(),
            r#"{"gpt-*": "gpt-4o"}"#.to_string(),
        );
        let result = mw.on_inbound_llm_request(&mut ctx, chat_request("gpt-3.5-turbo"))?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }
}

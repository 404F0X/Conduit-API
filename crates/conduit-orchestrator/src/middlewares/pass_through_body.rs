//! Pipeline middleware that captures the raw inbound request body for
//! pass-through forwarding. When pass-through is enabled, the outbound
//! request body is replaced with the raw client body (with model field
//! patched to the upstream model name).
//!
//! Go parity: `applyPassThroughRequestBody` (pass_through.go:76-115).

use conduit_llm::HttpRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Captures and forwards the raw client request body when pass-through is
/// enabled, patching only the model field for the upstream channel.
pub struct PassThroughRequestBodyMiddleware;

impl PipelineMiddleware for PassThroughRequestBodyMiddleware {
    fn name(&self) -> &'static str {
        "pass-through-request-body"
    }

    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        mut request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        // Store the outbound request for reference (Go: outbound.state.RawProviderRequest).
        if let Some(body) = &request.body {
            ctx.metadata
                .insert("raw_outbound_body_size".to_string(), body.len().to_string());
        }

        // Only apply pass-through body substitution when enabled.
        if ctx.metadata.get("pass_through_enabled").map(|s| s.as_str()) != Some("true") {
            return Ok(request);
        }

        // A raw body is wire-compatible only when the client protocol and the
        // selected candidate endpoint protocol are identical. Cross-protocol
        // attempts must keep the transformer's body (for example an OpenAI
        // Chat request cannot be copied onto `/v1/responses`). Missing metadata
        // fails closed; Pipeline stamps both values before this middleware.
        let client_format = ctx.metadata.get("client_api_format");
        let upstream_format = ctx.metadata.get("api_format");
        if client_format.is_none() || client_format != upstream_format {
            return Ok(request);
        }

        // Read the raw inbound body (stashed by the inbound transform step).
        let raw_body = match ctx.metadata.get("raw_inbound_body") {
            Some(body) if !body.is_empty() => body.clone(),
            _ => return Ok(request), // no raw body available
        };

        // Patch the model field in the raw body to match the upstream channel's model.
        let actual_model = ctx
            .metadata
            .get("actual_model")
            .cloned()
            .unwrap_or_default();

        let patched_body = if !actual_model.is_empty() {
            // Parse as JSON, replace model field, re-serialize.
            match serde_json::from_str::<serde_json::Value>(&raw_body) {
                Ok(mut json) => {
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert("model".to_string(), serde_json::Value::String(actual_model));
                    }
                    serde_json::to_vec(&json).unwrap_or_else(|_| raw_body.into_bytes())
                }
                Err(_) => raw_body.into_bytes(),
            }
        } else {
            raw_body.into_bytes()
        };

        request.body = Some(patched_body);
        ctx.metadata
            .insert("pass_through_body_applied".to_string(), "true".to_string());

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn disabled_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughRequestBodyMiddleware;
        let mut ctx = PipelineContext::new();
        let req = HttpRequest {
            body: Some(b"original".to_vec()),
            ..Default::default()
        };
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(result.body, Some(b"original".to_vec()));
        Ok(())
    }

    #[test]
    fn enabled_substitutes_raw_body_with_model_patch() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughRequestBodyMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_enabled".to_string(), "true".to_string());
        ctx.metadata.insert(
            "client_api_format".to_string(),
            "openai/chat_completions".to_string(),
        );
        ctx.metadata.insert(
            "api_format".to_string(),
            "openai/chat_completions".to_string(),
        );
        ctx.metadata.insert(
            "raw_inbound_body".to_string(),
            json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]}).to_string(),
        );
        ctx.metadata
            .insert("actual_model".to_string(), "gpt-4o-upstream".to_string());
        let req = HttpRequest {
            body: Some(b"outbound-transformed".to_vec()),
            ..Default::default()
        };
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        let body: serde_json::Value =
            serde_json::from_slice(result.body.as_deref().unwrap_or(&[]))?;
        assert_eq!(body["model"], json!("gpt-4o-upstream"));
        assert_eq!(body["messages"][0]["content"], json!("hi"));
        Ok(())
    }

    #[test]
    fn enabled_does_not_copy_raw_body_across_protocols() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PassThroughRequestBodyMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata
            .insert("pass_through_enabled".to_string(), "true".to_string());
        ctx.metadata.insert(
            "client_api_format".to_string(),
            "openai/chat_completions".to_string(),
        );
        ctx.metadata
            .insert("api_format".to_string(), "openai/responses".to_string());
        ctx.metadata.insert(
            "raw_inbound_body".to_string(),
            json!({"model": "public-model", "messages": []}).to_string(),
        );
        let transformed = json!({"model": "actual-model", "input": "hello"});
        let req = HttpRequest {
            body: Some(serde_json::to_vec(&transformed)?),
            ..Default::default()
        };

        let result = mw.on_outbound_raw_request(&mut ctx, req)?;

        assert_eq!(
            result.body.as_deref(),
            Some(serde_json::to_vec(&transformed)?.as_slice())
        );
        assert!(!ctx.metadata.contains_key("pass_through_body_applied"));
        Ok(())
    }
}

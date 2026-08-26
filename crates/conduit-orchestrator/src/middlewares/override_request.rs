//! Pipeline middleware that applies channel-configured body/header overrides
//! to outbound requests. Uses the pure functions from [`body_override`].
//!
//! Go parity: `applyOverrideRequestBody` + `applyOverrideRequestHeaders`
//! (orchestrator/override.go:130-465).

use conduit_core::objects::channel_settings::HeaderEntry;
use conduit_core::objects::overrides::OverrideOperation;
use conduit_llm::HttpRequest;
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

use super::body_override;

/// Middleware that applies body + header overrides from channel config.
/// Reads override operations from `ctx.metadata["channel_body_overrides"]`
/// (JSON-encoded Vec<OverrideOperation>) and applies them to the outbound
/// request body + headers.
pub struct OverrideRequestMiddleware;

impl PipelineMiddleware for OverrideRequestMiddleware {
    fn name(&self) -> &'static str {
        "override-request"
    }

    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        mut request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        // Read channel override config from context (set by orchestrator when
        // selecting channel). JSON-encoded Vec<OverrideOperation>.
        let body_ops: Vec<OverrideOperation> = ctx
            .metadata
            .get("channel_body_overrides")
            .filter(|json| !json.is_empty())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();

        // Header override operations from channel_settings.header_override_operations.
        let header_ops: Vec<OverrideOperation> = ctx
            .metadata
            .get("channel_header_overrides")
            .filter(|json| !json.is_empty())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();

        // Static header overrides from channel_settings.override_headers.
        let static_headers: Vec<HeaderEntry> = ctx
            .metadata
            .get("channel_override_headers")
            .filter(|json| !json.is_empty())
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();

        if body_ops.is_empty() && header_ops.is_empty() && static_headers.is_empty() {
            // Check override_parameters separately since it's a raw JSON merge.
            let override_params = ctx
                .metadata
                .get("channel_override_parameters")
                .filter(|s| !s.is_empty())
                .cloned();

            if override_params.is_none() {
                return Ok(request);
            }

            // Apply override_parameters: merge JSON keys into the request body.
            if let Some(params_json) = override_params
                && let Some(ref mut json_body) = request.json_body
                && let Ok(params) = serde_json::from_str::<serde_json::Value>(&params_json)
                && let (Some(body_map), Some(params_map)) =
                    (json_body.as_object_mut(), params.as_object())
            {
                for (key, value) in params_map {
                    body_map.insert(key.clone(), value.clone());
                }
            }

            return Ok(request);
        }

        // Apply override_parameters first (Go processes them before body overrides).
        if let Some(params_json) = ctx
            .metadata
            .get("channel_override_parameters")
            .filter(|s| !s.is_empty())
            && let Some(ref mut json_body) = request.json_body
            && let Ok(params) = serde_json::from_str::<serde_json::Value>(params_json)
            && let (Some(body_map), Some(params_map)) =
                (json_body.as_object_mut(), params.as_object())
        {
            for (key, value) in params_map {
                body_map.insert(key.clone(), value.clone());
            }
        }

        // Apply body overrides (if body exists). A single op failing (bad path,
        // type mismatch, out-of-range index) must not abort the others, but it
        // must not be silent either — log it so a mis-configured override is
        // diagnosable instead of the request going out unmodified with no trace
        // (P-39).
        if let Some(ref mut json_body) = request.json_body {
            for op in &body_ops {
                if let Err(err) = body_override::apply_body_operation(json_body, op) {
                    tracing::warn!(
                        op = %op.op,
                        path = %op.path,
                        error = %err,
                        "channel body override operation failed; leaving that field unchanged"
                    );
                }
            }
        }

        // Apply body_override_operations that target headers (op starts with "header_").
        if body_ops.iter().any(|op| op.op.starts_with("header_"))
            || !header_ops.is_empty()
            || !static_headers.is_empty()
        {
            let mut hm: std::collections::HashMap<String, String> = request
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for op in &body_ops {
                if op.op.starts_with("header_") {
                    body_override::apply_override_operation_to_headers(&mut hm, op);
                }
            }
            for op in &header_ops {
                body_override::apply_override_operation_to_headers(&mut hm, op);
            }
            for entry in &static_headers {
                if !entry.key.is_empty() {
                    hm.insert(entry.key.clone(), entry.value.clone());
                }
            }
            request.headers = hm.into_iter().collect();
        }

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn applies_body_set_from_context() -> Result<(), Box<dyn std::error::Error>> {
        let mw = OverrideRequestMiddleware;
        let mut ctx = PipelineContext::new();
        ctx.metadata.insert(
            "channel_body_overrides".to_string(),
            serde_json::to_string(&vec![OverrideOperation {
                op: "set".to_string(),
                path: "temperature".to_string(),
                value: "0.5".to_string(),
                ..Default::default()
            }])?,
        );
        let mut req = HttpRequest {
            json_body: Some(json!({"model": "gpt-4"})),
            ..Default::default()
        };
        req = mw.on_outbound_raw_request(&mut ctx, req)?;
        let body = req.json_body.as_ref().ok_or("no body")?;
        assert_eq!(body["temperature"], json!(0.5));
        Ok(())
    }

    #[test]
    fn no_overrides_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = OverrideRequestMiddleware;
        let mut ctx = PipelineContext::new();
        let req = HttpRequest {
            json_body: Some(json!({"model": "gpt-4"})),
            ..Default::default()
        };
        let result = mw.on_outbound_raw_request(&mut ctx, req)?;
        assert_eq!(result.json_body, Some(json!({"model": "gpt-4"})));
        Ok(())
    }
}

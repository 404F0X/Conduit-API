//! Pipeline middleware that splits reasoning-effort suffixes from model names
//! (e.g. "gpt-4o-high" → model "gpt-4o" + reasoning_effort "high"). Go parity:
//! `applyAutoReasoningEffort` (orchestrator/auto_reasoning_effort.go:21-99).

use std::sync::Arc;

use conduit_llm::{LlmRequest, LlmRequestPayload};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Source for the system model settings (decoupled from concrete service).
pub trait ModelSettingsSource: Send + Sync {
    /// Whether auto-reasoning-effort splitting is enabled in system settings.
    fn auto_reasoning_effort_enabled(&self) -> bool;
}

/// Middleware that splits "model-effort" suffixes into separate model +
/// reasoning_effort fields on the LlmRequest.
pub struct AutoReasoningEffortMiddleware<S: Send + Sync> {
    source: Arc<S>,
}

impl<S: Send + Sync> AutoReasoningEffortMiddleware<S> {
    pub fn new(source: Arc<S>) -> Self {
        Self { source }
    }
}

// Supported reasoning effort levels (Go: `supportedAutoReasoningEfforts`).
const SUPPORTED_EFFORTS: &[&str] = &["max", "xhigh", "high", "medium", "low"];

impl<S: ModelSettingsSource + 'static> PipelineMiddleware for AutoReasoningEffortMiddleware<S> {
    fn name(&self) -> &'static str {
        "auto-reasoning-effort"
    }

    fn on_inbound_llm_request(
        &self,
        _ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        let model = match &request.model {
            Some(m) if !m.is_empty() => m.clone(),
            _ => return Ok(request),
        };

        if !self.source.auto_reasoning_effort_enabled() {
            return Ok(request);
        }

        let Some((base, effort)) = split_auto_reasoning_effort_model(&model) else {
            return Ok(request);
        };

        request.model = Some(base.to_string());
        // Go sets `llmRequest.ReasoningEffort = reasoningEffort`; check if the
        // Rust LlmRequest has this field, otherwise stash in extra_body.
        if let LlmRequestPayload::Chat(chat) = &mut request.payload {
            chat.extra
                .insert("reasoning_effort".to_string(), serde_json::json!(effort));
        }

        Ok(request)
    }
}

/// Go parity: `splitAutoReasoningEffortModel` (auto_reasoning_effort.go:65-86).
fn split_auto_reasoning_effort_model(model: &str) -> Option<(&str, &str)> {
    let last_dash = model.rfind('-')?;
    if last_dash == 0 || last_dash == model.len() - 1 {
        return None;
    }

    let effort_str = &model[last_dash + 1..];
    let effort_lower = effort_str.to_ascii_lowercase();
    if !SUPPORTED_EFFORTS.contains(&effort_lower.as_str()) {
        return None;
    }

    // Go: `isQwenMaxModel` — skip "qwen*-max" models (Go: auto_reasoning_effort.go:88-99).
    if effort_lower == "max" && is_qwen_max_model(model) {
        return None;
    }

    let base = &model[..last_dash];
    if base.is_empty() {
        return None;
    }

    Some((base, effort_str))
}

/// Go parity: `isQwenMaxModel` (auto_reasoning_effort.go:88-99).
fn is_qwen_max_model(model: &str) -> bool {
    let normalized = model.trim().to_ascii_lowercase();
    if !normalized.ends_with("-max") {
        return false;
    }
    let after_slash = match normalized.rfind('/') {
        Some(pos) => &normalized[pos + 1..],
        None => &normalized,
    };
    after_slash.starts_with("qwen")
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::ChatRequest;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FakeSettings {
        enabled: AtomicBool,
    }

    impl FakeSettings {
        fn new(enabled: bool) -> Self {
            Self {
                enabled: AtomicBool::new(enabled),
            }
        }
    }

    impl ModelSettingsSource for FakeSettings {
        fn auto_reasoning_effort_enabled(&self) -> bool {
            self.enabled.load(Ordering::Relaxed)
        }
    }

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
    fn splits_effort_suffix() -> Result<(), Box<dyn std::error::Error>> {
        let mw = AutoReasoningEffortMiddleware::new(Arc::new(FakeSettings::new(true)));
        let req = chat_request("gpt-4o-high");
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }

    #[test]
    fn disabled_setting_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = AutoReasoningEffortMiddleware::new(Arc::new(FakeSettings::new(false)));
        let req = chat_request("gpt-4o-high");
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o-high"));
        Ok(())
    }

    #[test]
    fn unsupported_suffix_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = AutoReasoningEffortMiddleware::new(Arc::new(FakeSettings::new(true)));
        let req = chat_request("gpt-4o-turbo");
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        assert_eq!(result.model.as_deref(), Some("gpt-4o-turbo"));
        Ok(())
    }

    #[test]
    fn qwen_max_is_not_split() -> Result<(), Box<dyn std::error::Error>> {
        let mw = AutoReasoningEffortMiddleware::new(Arc::new(FakeSettings::new(true)));
        let req = chat_request("qwen-turbo-max");
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        assert_eq!(result.model.as_deref(), Some("qwen-turbo-max"));
        Ok(())
    }

    #[test]
    fn split_helper_unit_tests() {
        assert_eq!(
            split_auto_reasoning_effort_model("gpt-4o-high"),
            Some(("gpt-4o", "high"))
        );
        assert_eq!(
            split_auto_reasoning_effort_model("claude-3-opus-low"),
            Some(("claude-3-opus", "low"))
        );
        assert_eq!(split_auto_reasoning_effort_model("gpt-4o"), None);
        assert_eq!(split_auto_reasoning_effort_model("-high"), None);
        assert_eq!(split_auto_reasoning_effort_model("high-"), None);
        assert_eq!(split_auto_reasoning_effort_model("qwen-max"), None);
    }
}

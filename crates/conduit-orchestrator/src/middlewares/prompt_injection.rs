//! Pipeline middleware that injects system/developer prompts into the chat
//! message list before the request hits upstream. Go parity: `injectPrompts`
//! (orchestrator/prompt.go:31-75) + `PromptMatcher` (biz/prompt_matcher.go).

use std::sync::Arc;

use conduit_core::objects::prompt::PromptSettings;
use conduit_db::row::PromptRow;
use conduit_llm::{ChatMessage, LlmRequest, LlmRequestPayload, MessageContent};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Source of enabled prompts (decoupled from the concrete repo).
pub trait PromptSource: Send + Sync {
    fn list_enabled_prompts(&self, project_id: &str) -> Result<Vec<PromptRow>, String>;
}

/// Middleware that prepends/appends system prompts to inbound chat messages.
pub struct InjectPromptsMiddleware<S: Send + Sync> {
    source: Arc<S>,
}

impl<S: Send + Sync> InjectPromptsMiddleware<S> {
    pub fn new(source: Arc<S>) -> Self {
        Self { source }
    }
}

impl<S: PromptSource + 'static> PipelineMiddleware for InjectPromptsMiddleware<S> {
    fn name(&self) -> &'static str {
        "inject-prompts"
    }

    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        // Read project_id from context metadata (set by the auth middleware).
        let project_id = ctx
            .metadata
            .get("project_id")
            .map(|s| s.as_str())
            .unwrap_or("1")
            .to_string();

        let prompts = match self.source.list_enabled_prompts(&project_id) {
            Ok(p) => p,
            Err(_) => return Ok(request), // Go: warn + passthrough
        };
        if prompts.is_empty() {
            return Ok(request);
        }

        // Extract model name and api_key_id for condition matching.
        let model = request.model.clone().unwrap_or_default();
        let api_key_id: i64 = ctx
            .metadata
            .get("api_key_id")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);

        // Filter prompts whose activation conditions match (Go: FilterMatchingPrompts).
        let matching: Vec<&PromptRow> = prompts
            .iter()
            .filter(|p| prompt_matches(p, &model, api_key_id))
            .collect();
        if matching.is_empty() {
            return Ok(request);
        }

        // Sort by order ASC, then created_at ASC (Go: sort.SliceStable).
        let mut sorted: Vec<&PromptRow> = matching;
        sorted.sort_by(|a, b| {
            a.order_val
                .cmp(&b.order_val)
                .then(a.created_at.cmp(&b.created_at))
        });

        // Build prepend/append message lists (Go: ApplyPrompts).
        let mut prepend = Vec::new();
        let mut append = Vec::new();
        for p in &sorted {
            let msg = ChatMessage {
                role: p.role.clone(),
                content: Some(MessageContent::Text(p.content.clone())),
                tool_calls: Vec::new(),
                tool_call_id: None,
                name: None,
                extra: Default::default(),
            };
            let settings: PromptSettings =
                serde_json::from_value(p.settings.clone()).unwrap_or_default();
            if settings.action.kind == "append" {
                append.push(msg);
            } else {
                prepend.push(msg); // default = prepend
            }
        }

        // Inject into chat messages.
        if let LlmRequestPayload::Chat(chat) = &mut request.payload {
            let mut new_messages =
                Vec::with_capacity(prepend.len() + chat.messages.len() + append.len());
            new_messages.extend(prepend);
            new_messages.append(&mut chat.messages);
            new_messages.extend(append);
            chat.messages = new_messages;
        }

        Ok(request)
    }
}

/// Go parity: `PromptMatcher.MatchPrompt` — checks if a prompt's activation
/// conditions are satisfied for the given model and API key.
fn prompt_matches(prompt: &PromptRow, model: &str, api_key_id: i64) -> bool {
    if prompt.status != "enabled" {
        return false;
    }
    let settings: PromptSettings = match serde_json::from_value(prompt.settings.clone()) {
        Ok(s) => s,
        Err(_) => return false,
    };
    if settings.conditions.is_empty() {
        return true;
    }
    // All composite conditions must match (AND); each composite: at least one
    // inner condition must match (OR).
    settings.conditions.iter().all(|composite| {
        if composite.conditions.is_empty() {
            return true;
        }
        composite
            .conditions
            .iter()
            .any(|cond| match cond.kind.as_str() {
                "model_id" => cond.model_id.as_deref() == Some(model),
                "model_pattern" => {
                    let Some(pattern) = cond.model_pattern.as_deref() else {
                        return false;
                    };
                    regex::Regex::new(pattern)
                        .map(|re| re.is_match(model))
                        .unwrap_or(false)
                }
                "api_key" => cond.api_key_id == Some(api_key_id),
                _ => false,
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::ChatRequest;
    use serde_json::json;
    use std::sync::Mutex;

    struct InMemoryPromptSource {
        prompts: Mutex<Vec<PromptRow>>,
    }

    impl InMemoryPromptSource {
        fn new(prompts: Vec<PromptRow>) -> Self {
            Self {
                prompts: Mutex::new(prompts),
            }
        }
    }

    impl PromptSource for InMemoryPromptSource {
        fn list_enabled_prompts(&self, _project_id: &str) -> Result<Vec<PromptRow>, String> {
            Ok(self.prompts.lock().map_err(|e| e.to_string())?.clone())
        }
    }

    fn make_prompt(role: &str, content: &str, order: i64, action_type: &str) -> PromptRow {
        PromptRow {
            id: "1".to_string(),
            project_id: "1".to_string(),
            name: format!("prompt-{order}"),
            description: String::new(),
            role: role.to_string(),
            content: content.to_string(),
            order_val: order,
            status: "enabled".to_string(),
            settings: json!({ "action": { "type": action_type } }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
        }
    }

    fn chat_request(messages: Vec<ChatMessage>) -> LlmRequest {
        LlmRequest {
            request_type: conduit_llm::RequestType::Chat,
            api_format: conduit_llm::ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-4".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages,
                ..Default::default()
            }),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn text_msg(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn prepend_prompt_injected_before_user_messages() -> Result<(), Box<dyn std::error::Error>> {
        let mw =
            InjectPromptsMiddleware::new(Arc::new(InMemoryPromptSource::new(vec![make_prompt(
                "system",
                "You are helpful.",
                0,
                "prepend",
            )])));
        let req = chat_request(vec![text_msg("user", "Hello")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let msgs = match &result.payload {
            LlmRequestPayload::Chat(c) => &c.messages,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[1].role, "user");
        Ok(())
    }

    #[test]
    fn append_prompt_injected_after_user_messages() -> Result<(), Box<dyn std::error::Error>> {
        let mw =
            InjectPromptsMiddleware::new(Arc::new(InMemoryPromptSource::new(vec![make_prompt(
                "system", "Be safe.", 0, "append",
            )])));
        let req = chat_request(vec![text_msg("user", "Hello")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let msgs = match &result.payload {
            LlmRequestPayload::Chat(c) => &c.messages,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "system");
        Ok(())
    }

    #[test]
    fn empty_prompts_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = InjectPromptsMiddleware::new(Arc::new(InMemoryPromptSource::new(vec![])));
        let req = chat_request(vec![text_msg("user", "Hello")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let msgs = match &result.payload {
            LlmRequestPayload::Chat(c) => &c.messages,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(msgs.len(), 1);
        Ok(())
    }

    #[test]
    fn condition_model_id_filters_non_matching() -> Result<(), Box<dyn std::error::Error>> {
        let mut prompt = make_prompt("system", "Only for 3.5", 0, "prepend");
        prompt.settings = json!({
            "action": { "type": "prepend" },
            "conditions": [{
                "conditions": [{
                    "type": "model_id",
                    "modelId": "gpt-3.5-turbo"
                }]
            }]
        });
        let mw = InjectPromptsMiddleware::new(Arc::new(InMemoryPromptSource::new(vec![prompt])));
        let req = chat_request(vec![text_msg("user", "Hello")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let msgs = match &result.payload {
            LlmRequestPayload::Chat(c) => &c.messages,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(msgs.len(), 1, "non-matching model should not inject");
        Ok(())
    }
}

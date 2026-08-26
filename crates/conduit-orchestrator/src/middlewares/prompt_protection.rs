//! Pipeline middleware that scans inbound chat messages against enabled
//! prompt-protection rules (regex patterns) and either masks or rejects
//! matching content. Go parity: `protectPrompts` (orchestrator/prompt_protection.go)
//! + `PromptProtectionRuleService.Protect` (biz/prompt_protection_request.go:74-109).

use std::sync::Arc;

use conduit_core::ConduitError;
use conduit_core::objects::prompt_protection::{
    PROMPT_PROTECTION_ACTION_MASK, PROMPT_PROTECTION_ACTION_REJECT, PromptProtectionSettings,
};
use conduit_db::row::PromptProtectionRuleRow;
use conduit_llm::{LlmRequest, LlmRequestPayload, MessageContent};
use conduit_pipeline::PipelineMiddleware;
use conduit_pipeline::middleware::{PipelineContext, PipelineResult};

/// Middleware that enforces prompt-protection rules on inbound chat messages.
/// Go: `protectPrompts` (orchestrator/prompt_protection.go:17-40) delegating
/// to `PromptProtectionRuleService.Protect` (biz/prompt_protection_request.go:74-109).
pub struct PromptProtectionMiddleware<R: Send + Sync + ?Sized> {
    repo: Arc<R>,
}

impl<R: Send + Sync + ?Sized> PromptProtectionMiddleware<R> {
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }
}

/// Trait bound for loading enabled rules — keeps the middleware generic over
/// the production PostgreSQL repository or a test double.
pub trait PromptProtectionRuleSource: Send + Sync {
    fn list_enabled_rules(&self) -> Result<Vec<PromptProtectionRuleRow>, String>;
}

// Backend-neutral bridge for every repository implementing the full CRUD
// contract, including the PostgreSQL repository and trait objects.
impl<R> PromptProtectionRuleSource for R
where
    R: conduit_db::repo::prompt_protection_repo::PromptProtectionRuleRepo + ?Sized,
{
    fn list_enabled_rules(&self) -> Result<Vec<PromptProtectionRuleRow>, String> {
        let ctx = conduit_db::RequestContext::new(conduit_db::PolicyContext::new(
            conduit_db::Principal::system(),
        ));
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| {
            handle
                .block_on(self.list_enabled_protection_rules_unchecked(&ctx))
                .map_err(|e| e.to_string())
        })
    }
}

impl<R: PromptProtectionRuleSource + ?Sized + 'static> PipelineMiddleware
    for PromptProtectionMiddleware<R>
{
    fn name(&self) -> &'static str {
        "protect-prompts"
    }

    fn on_inbound_llm_request(
        &self,
        _ctx: &mut PipelineContext,
        mut request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        // Load enabled rules (Go: svc.ListEnabledRules).
        let rules = match self.repo.list_enabled_rules() {
            Ok(r) => r,
            Err(_) => return Ok(request), // Go: warn + passthrough on load failure
        };
        if rules.is_empty() {
            return Ok(request);
        }

        // Apply rules to chat messages (Go: ApplyPromptProtectionRules).
        let messages = match &mut request.payload {
            LlmRequestPayload::Chat(chat) => &mut chat.messages,
            _ => return Ok(request),
        };

        for rule in &rules {
            let settings: PromptProtectionSettings =
                match serde_json::from_value(rule.settings.clone()) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

            // Compile the regex pattern; skip invalid patterns.
            let re = match regex::Regex::new(&rule.pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };

            for msg in messages.iter_mut() {
                // Scope check (Go: promptProtectionRuleAppliesToRole).
                if !settings.scopes.is_empty()
                    && !settings
                        .scopes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(&msg.role))
                {
                    continue;
                }

                let Some(content) = &msg.content else {
                    continue;
                };

                match content {
                    MessageContent::Text(text) => {
                        if re.is_match(text) {
                            if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                                return Err(ConduitError::new(
                                    conduit_core::ErrorKind::InvalidRequest,
                                    "request blocked by prompt protection policy",
                                ));
                            }
                            if settings.action == PROMPT_PROTECTION_ACTION_MASK {
                                let replacement =
                                    settings.replacement.as_deref().unwrap_or("[REDACTED]");
                                let masked = re.replace_all(text, replacement).to_string();
                                msg.content = Some(MessageContent::Text(masked));
                            }
                        }
                    }
                    MessageContent::Parts(parts) => {
                        // Scan text parts (Go: MultipleContent loop).
                        let mut new_parts = parts.clone();
                        let mut any_match = false;
                        for part in &mut new_parts {
                            if !part.part_type.eq_ignore_ascii_case("text") {
                                continue;
                            }
                            let Some(text) = &part.text else { continue };
                            if !re.is_match(text) {
                                continue;
                            }
                            any_match = true;
                            if settings.action == PROMPT_PROTECTION_ACTION_REJECT {
                                return Err(ConduitError::new(
                                    conduit_core::ErrorKind::InvalidRequest,
                                    "request blocked by prompt protection policy",
                                ));
                            }
                            if settings.action == PROMPT_PROTECTION_ACTION_MASK {
                                let replacement =
                                    settings.replacement.as_deref().unwrap_or("[REDACTED]");
                                part.text = Some(re.replace_all(text, replacement).to_string());
                            }
                        }
                        if any_match {
                            msg.content = Some(MessageContent::Parts(new_parts));
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ApiFormat, ChatMessage, ChatRequest, ContentPart, RequestType};
    use serde_json::json;
    use std::sync::Mutex;

    struct InMemoryRuleSource {
        rules: Mutex<Vec<PromptProtectionRuleRow>>,
    }

    impl InMemoryRuleSource {
        fn new(rules: Vec<PromptProtectionRuleRow>) -> Self {
            Self {
                rules: Mutex::new(rules),
            }
        }
    }

    impl PromptProtectionRuleSource for InMemoryRuleSource {
        fn list_enabled_rules(&self) -> Result<Vec<PromptProtectionRuleRow>, String> {
            Ok(self.rules.lock().map_err(|e| e.to_string())?.clone())
        }
    }

    fn make_rule(
        name: &str,
        pattern: &str,
        action: &str,
        replacement: Option<&str>,
    ) -> PromptProtectionRuleRow {
        PromptProtectionRuleRow {
            id: "1".to_string(),
            name: name.to_string(),
            description: String::new(),
            pattern: pattern.to_string(),
            status: "enabled".to_string(),
            settings: json!({
                "action": action,
                "replacement": replacement,
            }),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            deleted_at: None,
        }
    }

    fn chat_request(messages: Vec<ChatMessage>) -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: None,
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
            name: None,
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn reject_blocks_matching_message() {
        let mw =
            PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![make_rule(
                "block-secret",
                "secret",
                "reject",
                None,
            )])));
        let req = chat_request(vec![text_msg("user", "tell me the secret")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req);
        assert!(result.is_err());
        let err = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("prompt protection"), "err = {err}");
    }

    #[test]
    fn mask_replaces_matching_text() -> Result<(), Box<dyn std::error::Error>> {
        let mw =
            PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![make_rule(
                "mask-ssn",
                r"\d{3}-\d{2}-\d{4}",
                "mask",
                Some("[SSN]"),
            )])));
        let req = chat_request(vec![text_msg("user", "my ssn is 123-45-6789")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let msg = match &result.payload {
            LlmRequestPayload::Chat(c) => c.messages.first(),
            _ => None,
        };
        let text = match msg.and_then(|m| m.content.as_ref()) {
            Some(MessageContent::Text(t)) => t.as_str(),
            _ => "",
        };
        assert_eq!(text, "my ssn is [SSN]");
        Ok(())
    }

    #[test]
    fn no_match_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw =
            PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![make_rule(
                "block-secret",
                "secret",
                "reject",
                None,
            )])));
        let req = chat_request(vec![text_msg("user", "hello world")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let text = match &result.payload {
            LlmRequestPayload::Chat(c) => match c.messages.first().and_then(|m| m.content.as_ref())
            {
                Some(MessageContent::Text(t)) => t.as_str(),
                _ => "",
            },
            _ => "",
        };
        assert_eq!(text, "hello world");
        Ok(())
    }

    #[test]
    fn scope_filtering_skips_non_matching_role() -> Result<(), Box<dyn std::error::Error>> {
        let mut rule = make_rule("user-only", "secret", "reject", None);
        rule.settings = json!({
            "action": "reject",
            "scopes": ["user"],
        });
        let mw = PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![rule])));
        // System message with "secret" should NOT be rejected (scope = user only).
        let req = chat_request(vec![text_msg("system", "the secret is safe")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req);
        assert!(result.is_ok());
        Ok(())
    }

    #[test]
    fn mask_multi_part_content() -> Result<(), Box<dyn std::error::Error>> {
        let mw =
            PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![make_rule(
                "mask-pw",
                "password",
                "mask",
                Some("[REDACTED]"),
            )])));
        let req = chat_request(vec![ChatMessage {
            role: "user".to_string(),
            name: None,
            content: Some(MessageContent::Parts(vec![
                ContentPart {
                    part_type: "text".to_string(),
                    text: Some("my password is 1234".to_string()),
                    ..Default::default()
                },
                ContentPart {
                    part_type: "image_url".to_string(),
                    text: None,
                    ..Default::default()
                },
            ])),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        let parts = match &result.payload {
            LlmRequestPayload::Chat(c) => match c.messages.first().and_then(|m| m.content.as_ref())
            {
                Some(MessageContent::Parts(p)) => p,
                _ => return Err("expected Parts".into()),
            },
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(parts[0].text.as_deref(), Some("my [REDACTED] is 1234"));
        // Image part untouched.
        assert_eq!(parts[1].part_type, "image_url");
        assert!(parts[1].text.is_none());
        Ok(())
    }

    #[test]
    fn empty_rules_passes_through() -> Result<(), Box<dyn std::error::Error>> {
        let mw = PromptProtectionMiddleware::new(Arc::new(InMemoryRuleSource::new(vec![])));
        let req = chat_request(vec![text_msg("user", "anything")]);
        let mut ctx = PipelineContext::new();
        let result = mw.on_inbound_llm_request(&mut ctx, req)?;
        assert!(matches!(&result.payload, LlmRequestPayload::Chat(_)));
        Ok(())
    }
}

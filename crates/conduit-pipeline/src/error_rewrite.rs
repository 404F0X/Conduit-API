use conduit_core::objects::channel_settings::ErrorResponseRewriteRule;
use conduit_core::{
    ConduitError, ERROR_RESPONSE_BODY_METADATA, ERROR_RESPONSE_REWRITE_CHANNEL_METADATA,
    ERROR_RESPONSE_TYPE_METADATA, ErrorKind,
};
use regex::Regex;
use serde_json::Value;

/// Apply the first matching channel rule to the final client-visible error.
/// Original diagnostic fields (`message`, `provider_status`, `provider_body`)
/// remain available to retry, persistence and logs.
pub fn apply_error_response_rewrite(
    channel_id: &str,
    rules: &[ErrorResponseRewriteRule],
    mut error: ConduitError,
) -> ConduitError {
    if error.kind != ErrorKind::Upstream {
        return error;
    }

    let Some(rule) = rules.iter().find(|rule| rule_matches(rule, &error)) else {
        return error;
    };

    let context = RewriteContext::new(channel_id, &error);
    if let Some(status) = rule.http_status {
        error.http_status = status;
    }
    if let Some(message) = rule.message.as_deref() {
        error.safe_message = Some(expand_string(message, &context));
    }
    if let Some(error_type) = rule.error_type.as_deref() {
        error.metadata.insert(
            ERROR_RESPONSE_TYPE_METADATA.to_string(),
            Value::String(expand_string(error_type, &context)),
        );
    }
    if let Some(code) = rule.code.as_deref() {
        error.code = Some(expand_string(code, &context));
    }
    if let Some(body) = rule.body.as_ref() {
        error.metadata.insert(
            ERROR_RESPONSE_BODY_METADATA.to_string(),
            expand_value(body, &context),
        );
    }
    error.metadata.insert(
        ERROR_RESPONSE_REWRITE_CHANNEL_METADATA.to_string(),
        Value::String(channel_id.to_string()),
    );
    error
}

fn rule_matches(rule: &ErrorResponseRewriteRule, error: &ConduitError) -> bool {
    let status = error.provider_status.unwrap_or(error.http_status);
    if !rule.status_codes.is_empty() && !rule.status_codes.contains(&status) {
        return false;
    }
    if rule.body_pattern.is_empty() {
        return true;
    }
    let Ok(pattern) = Regex::new(&rule.body_pattern) else {
        // Administrative writes validate regexes. A legacy/manual malformed
        // settings blob must fail closed instead of rewriting every error.
        return false;
    };
    let text = error
        .provider_body
        .as_ref()
        .map(Value::to_string)
        .unwrap_or_else(|| error.message.clone());
    pattern.is_match(&text)
}

struct RewriteContext {
    channel_id: String,
    message: String,
    status: String,
    provider_status: String,
    code: String,
}

impl RewriteContext {
    fn new(channel_id: &str, error: &ConduitError) -> Self {
        Self {
            channel_id: channel_id.to_string(),
            message: error.message.clone(),
            status: error.http_status.to_string(),
            provider_status: error
                .provider_status
                .map(|status| status.to_string())
                .unwrap_or_default(),
            code: error.code.clone().unwrap_or_default(),
        }
    }
}

fn expand_string(template: &str, context: &RewriteContext) -> String {
    template
        .replace("${channel_id}", &context.channel_id)
        .replace("${provider_status}", &context.provider_status)
        .replace("${status}", &context.status)
        .replace("${message}", &context.message)
        .replace("${code}", &context.code)
}

fn expand_value(value: &Value, context: &RewriteContext) -> Value {
    match value {
        Value::String(value) => Value::String(expand_string(value, context)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| expand_value(value, context))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), expand_value(value, context)))
                .collect(),
        ),
        value => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn first_matching_rule_rewrites_only_client_fields() {
        let original = ConduitError::upstream("provider secret")
            .with_provider_status(429)
            .with_code("rate_limit")
            .with_provider_body(json!({"error":"capacity"}));
        let rules = vec![ErrorResponseRewriteRule {
            status_codes: vec![429],
            body_pattern: "capacity".into(),
            http_status: Some(503),
            message: Some("channel ${channel_id}: retry later".into()),
            error_type: Some("overloaded_error".into()),
            code: Some("temporarily_unavailable".into()),
            body: Some(json!({
                "error": {
                    "message": "upstream ${provider_status}",
                    "channel": "${channel_id}"
                }
            })),
        }];

        let rewritten = apply_error_response_rewrite("17", &rules, original);
        assert_eq!(rewritten.http_status, 503);
        assert_eq!(rewritten.provider_status, Some(429));
        assert_eq!(rewritten.message, "provider secret");
        assert_eq!(rewritten.public_message(), "channel 17: retry later");
        assert_eq!(rewritten.code.as_deref(), Some("temporarily_unavailable"));
        assert_eq!(
            rewritten.metadata[ERROR_RESPONSE_BODY_METADATA],
            json!({"error":{"message":"upstream 429","channel":"17"}})
        );
    }

    #[test]
    fn malformed_regex_fails_closed_and_non_upstream_errors_are_ignored() {
        let rule = ErrorResponseRewriteRule {
            body_pattern: "(".into(),
            message: Some("hidden".into()),
            ..Default::default()
        };
        let upstream = apply_error_response_rewrite(
            "1",
            std::slice::from_ref(&rule),
            ConduitError::upstream("raw"),
        );
        assert_ne!(upstream.public_message(), "hidden");
        let invalid = apply_error_response_rewrite(
            "1",
            &[ErrorResponseRewriteRule {
                body_pattern: String::new(),
                ..rule
            }],
            ConduitError::invalid_request("bad request"),
        );
        assert_eq!(invalid.public_message(), "bad request");
    }
}

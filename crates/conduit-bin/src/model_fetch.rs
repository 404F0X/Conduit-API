//! Shared upstream model-list fetcher — ports the OpenAI-compatible +
//! Anthropic + Gemini branches of Go `biz.ModelFetcher.FetchModels`
//! (`biz/model_fetcher.go`). Backs both the admin GraphQL `fetchModels` query
//! (`ModelExtAdapter::fetch_models`) and the `syncChannelModels` mutation
//! (`ChannelExtMutationAdapter::sync_channel_models`).
//!
//! ## Scope
//! Handles the three wire families the host already proxies:
//!   * OpenAI-compatible (`{base}/models`, `Authorization: Bearer`, response
//!     `{ "data": [ { "id": .. } ] }`)
//!   * Anthropic (`{base}/v1/models`, `x-api-key` + `anthropic-version`)
//!   * Gemini (`{base}/v1beta/models`, `x-goog-api-key`, response
//!     `{ "models": [ { "name": "models/.." } ] }`)
//!
//! Provider-specific fallbacks Go carries (Qiniu static list, GitHub catalog,
//! Copilot cached conf, OAuth default-model tables) are out of scope; those
//! channel types return an empty list with a descriptive note rather than a
//! fabricated catalog.

use std::collections::HashSet;
use std::time::Duration;

const MAX_MODEL_PAGES: usize = 100;

/// Result of a model-list fetch — mirrors Go `FetchModelsResult`
/// (`biz/model_fetcher.go`): a list of model ids plus an optional soft error
/// string (Go returns the error in-band so the resolver can surface it without
/// failing the whole request).
pub struct FetchedModels {
    pub model_ids: Vec<String>,
    pub error: Option<String>,
}

impl FetchedModels {
    fn ok(ids: Vec<String>) -> Self {
        Self {
            model_ids: ids,
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            model_ids: Vec::new(),
            error: Some(msg.into()),
        }
    }
}

/// `true` for Anthropic-family channel types (native `/v1/models` + `x-api-key`).
fn is_anthropic(channel_type: &str) -> bool {
    matches!(channel_type, "anthropic" | "claudecode")
}

/// `true` for Gemini-family channel types.
fn is_gemini(channel_type: &str) -> bool {
    channel_type == "gemini"
}

/// Build the models endpoint URL for a channel type + base_url, mirroring the
/// relevant branches of Go `prepareModelsEndpoint` (`model_fetcher.go`). The
/// base_url typically already carries the provider's version segment
/// (`https://host/v1`), so `/models` is appended when a `/v1`-style segment is
/// already present, else the versioned path is added.
fn models_endpoint(channel_type: &str, base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if is_gemini(channel_type) {
        if base.contains("/v1") {
            format!("{base}/models")
        } else {
            format!("{base}/v1beta/models")
        }
    } else if is_anthropic(channel_type) {
        let base = base
            .trim_end_matches("/anthropic")
            .trim_end_matches("/claude");
        if base.ends_with("/v1") {
            format!("{base}/models")
        } else {
            format!("{base}/v1/models")
        }
    } else {
        // OpenAI-compatible default (model_fetcher.go default branch).
        if base.contains("/v1") {
            format!("{base}/models")
        } else {
            format!("{base}/v1/models")
        }
    }
}

/// Parse an OpenAI-compatible / Anthropic model-list body: `{ "data": [ { "id":
/// .. } ] }`. Also tolerates a bare top-level array of `{ "id": .. }`
/// (Go `parseModelsResponse` direct-array branch).
fn parse_openai_models(json: &serde_json::Value) -> Result<Vec<String>, String> {
    let arr = json
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| json.as_array())
        .ok_or_else(|| "models response must contain a data array".to_string())?;
    arr.iter()
        .enumerate()
        .map(|(index, model)| {
            model
                .get("id")
                .and_then(|value| value.as_str())
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("models response data[{index}] has no valid id"))
        })
        .collect()
}

/// Parse a Gemini model-list body: `{ "models": [ { "name": "models/.." } ] }`,
/// stripping the `models/` prefix (Go `parseModelsResponse` Gemini branch).
fn parse_gemini_models(json: &serde_json::Value) -> Result<Vec<String>, String> {
    let models = json
        .get("models")
        .and_then(|m| m.as_array())
        .ok_or_else(|| "models response must contain a models array".to_string())?;
    models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            model
                .get("name")
                .and_then(|value| value.as_str())
                .map(|name| name.trim_start_matches("models/"))
                .filter(|name| !name.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("models response models[{index}] has no valid name"))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ModelPageCursor {
    Anthropic(String),
    Gemini(String),
}

fn next_page_cursor(
    channel_type: &str,
    json: &serde_json::Value,
) -> Result<Option<ModelPageCursor>, String> {
    if is_anthropic(channel_type) {
        let Some(has_more) = json.get("has_more") else {
            return Ok(None);
        };
        let has_more = has_more
            .as_bool()
            .ok_or_else(|| "models response has invalid has_more".to_string())?;
        if !has_more {
            return Ok(None);
        }
        let last_id = json
            .get("last_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "paginated models response has no last_id".to_string())?;
        return Ok(Some(ModelPageCursor::Anthropic(last_id.to_string())));
    }
    if is_gemini(channel_type) {
        return match json.get("nextPageToken") {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(value) => value
                .as_str()
                .ok_or_else(|| "models response has invalid nextPageToken".to_string())
                .map(|token| {
                    (!token.trim().is_empty()).then(|| ModelPageCursor::Gemini(token.to_string()))
                }),
        };
    }
    Ok(None)
}

/// Fetch the provider's model list for a channel. Returns model ids (Go returns
/// them de-duplicated; the caller merges with manual models). Network/parse
/// failures come back as a soft `error` (empty id list), matching Go's in-band
/// error contract so callers can surface a message without a hard failure.
pub async fn fetch_models(
    client: &reqwest::Client,
    channel_type: &str,
    base_url: &str,
    api_key: &str,
) -> FetchedModels {
    if base_url.trim().is_empty() {
        return FetchedModels::err("channel has no base URL configured");
    }
    if api_key.trim().is_empty() {
        return FetchedModels::err("API key is required");
    }

    let url = models_endpoint(channel_type, base_url);
    let mut ids = Vec::new();
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    for page in 0..MAX_MODEL_PAGES {
        let mut req = client.get(&url).timeout(Duration::from_secs(15));
        if is_anthropic(channel_type) {
            req = req
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .query(&[("limit", "1000")]);
            if let Some(ModelPageCursor::Anthropic(after_id)) = cursor.as_ref() {
                req = req.query(&[("after_id", after_id)]);
            }
        } else if is_gemini(channel_type) {
            req = req
                .header("x-goog-api-key", api_key)
                .query(&[("pageSize", "1000")]);
            if let Some(ModelPageCursor::Gemini(page_token)) = cursor.as_ref() {
                req = req.query(&[("pageToken", page_token)]);
            }
        } else {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }

        let resp = match req.send().await {
            Ok(response) => response,
            Err(error) => {
                return FetchedModels::err(format!("failed to fetch models: {error}"));
            }
        };
        let status = resp.status();
        let text = match resp.text().await {
            Ok(text) => text,
            Err(error) => {
                return FetchedModels::err(format!("failed to read models response: {error}"));
            }
        };
        if !status.is_success() {
            let json: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let message = json
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(|message| message.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("failed to fetch models: status {}", status.as_u16()));
            return FetchedModels::err(message);
        }

        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                return FetchedModels::err(format!("failed to parse models response: {error}"));
            }
        };
        let page_ids = if is_gemini(channel_type) {
            parse_gemini_models(&json)
        } else {
            parse_openai_models(&json)
        };
        let page_ids = match page_ids {
            Ok(page_ids) => page_ids,
            Err(error) => return FetchedModels::err(error),
        };
        ids.extend(page_ids);

        cursor = match next_page_cursor(channel_type, &json) {
            Ok(cursor) => cursor,
            Err(error) => return FetchedModels::err(error),
        };
        let Some(next) = cursor.as_ref() else {
            break;
        };
        if !seen_cursors.insert(next.clone()) {
            return FetchedModels::err("models response repeated a pagination cursor");
        }
        if page + 1 == MAX_MODEL_PAGES {
            return FetchedModels::err("models response exceeded the pagination limit");
        }
    }

    // De-duplicate preserving first-seen order (Go `lo.Uniq`).
    let mut seen = HashSet::new();
    let deduped: Vec<String> = ids
        .into_iter()
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect();
    FetchedModels::ok(deduped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_openai_compat_with_v1() {
        assert_eq!(
            models_endpoint("openai", "https://muyuan.do/v1"),
            "https://muyuan.do/v1/models"
        );
        assert_eq!(
            models_endpoint("deepseek", "https://api.deepseek.com"),
            "https://api.deepseek.com/v1/models"
        );
    }

    #[test]
    fn endpoint_anthropic_and_gemini() {
        assert_eq!(
            models_endpoint("anthropic", "https://api.anthropic.com"),
            "https://api.anthropic.com/v1/models"
        );
        assert_eq!(
            models_endpoint("gemini", "https://generativelanguage.googleapis.com"),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
    }

    #[test]
    fn parse_openai_data_array() {
        let v = serde_json::json!({"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]});
        assert_eq!(
            parse_openai_models(&v).unwrap(),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
    }

    #[test]
    fn parse_gemini_strips_prefix() {
        let v = serde_json::json!({"models":[{"name":"models/gemini-1.5-pro"}]});
        assert_eq!(parse_gemini_models(&v).unwrap(), vec!["gemini-1.5-pro"]);
    }

    #[test]
    fn unexpected_success_payload_is_not_treated_as_an_empty_catalog() {
        let response = serde_json::json!({"status":"temporarily unavailable"});

        assert!(parse_openai_models(&response).is_err());
        assert!(parse_gemini_models(&response).is_err());
    }

    #[test]
    fn provider_pagination_requires_and_returns_a_cursor() {
        let anthropic = serde_json::json!({
            "data": [{"id":"claude-1"}],
            "has_more": true,
            "last_id": "claude-1"
        });
        assert_eq!(
            next_page_cursor("anthropic", &anthropic).unwrap(),
            Some(ModelPageCursor::Anthropic("claude-1".into()))
        );

        let malformed = serde_json::json!({"data": [], "has_more": true});
        assert!(next_page_cursor("anthropic", &malformed).is_err());

        let gemini = serde_json::json!({
            "models": [{"name":"models/gemini-1"}],
            "nextPageToken": "next"
        });
        assert_eq!(
            next_page_cursor("gemini", &gemini).unwrap(),
            Some(ModelPageCursor::Gemini("next".into()))
        );
    }
}

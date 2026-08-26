//! ADPT-CHANNEL-EXT — host adapter wiring the admin GraphQL Channel
//! *extended* + *bulk* mutation slices to the configured channel repositories.
//!
//! Backs the two host-injected traits:
//!   - [`ChannelExtMutationServices`] (channel_ext.rs:191) —
//!     `updateChannelStatus` / `duplicateChannel` / `saveChannelEndpoints` /
//!     `testChannel*` / `bulk{Archive,Disable,Enable,Recover,Delete}Channels` /
//!     `syncChannelModels`.
//!   - [`ChannelBulkMutationServices`] (channel_ext2.rs:228) —
//!     `bulkCreateChannels` / `bulkImportChannels` /
//!     `bulkUpdateChannelOrdering` / per-channel API-key management.
//!
//! Deliberately distinct from the sibling `ChannelCrudAdapter`
//! (`wiring_channel_crud.rs`, base CRUD trio + `Query.channels`); the enum/
//! input lowering helpers are duplicated here because they are private to that
//! module (keep the two copies in sync).
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//!   - `UpdateChannelStatus` (`biz/channel.go:771`): bare
//!     `UpdateOneID(id).SetStatus(status)`.
//!   - `DuplicateChannel` (`biz/channel_duplicate.go:17`): verify the source
//!     exists, duplicate-name check, then `createChannel` with the provided
//!     `CreateChannelInput` (the input carries the full clone payload).
//!     Current model prices are copied into a unified ChangeSet draft in the
//!     same transaction; formal prices remain empty until approval.
//!   - `SaveChannelEndpoints` (`biz/channel.go:800`): `ValidateEndpoints` then
//!     `SetEndpoints`. The structural validations (api_format required, no
//!     duplicate api_format, path shape) are ported; DEFER: the
//!     `SupportedAPIFormats` whitelist + websocket-transport checks need the
//!     `llm` API-format constants which are not ported yet — replicating the
//!     list here would risk synthesizing it.
//!   - `bulkUpdateChannelStatus` (`biz/channel_bulk.go:137`): verify every id
//!     exists (count == len) before updating; recover = enabled +
//!     `ClearErrorMessage` (it does NOT clear `deleted_at`).
//!   - `BulkDeleteChannels` (`biz/channel_bulk.go:194`): `Delete().Where(IDIn)`
//!     — no existence check; the `SoftDeleteMixin` makes it a soft delete, so
//!     missing ids are silently skipped.
//!   - `BulkCreateChannels` (`biz/channel_bulk.go:61`): api-keys + baseURL
//!     required; one channel per key named `"{name} - ({n})"` skipping taken
//!     names; tags default to `[name]`; credentials `{apiKeys: [key]}`.
//!   - `BulkImportChannels` (`biz/channel_bulk.go:234`): best-effort row loop,
//!     per-row `"Row %d ..."` error strings, `success = failed == 0`.
//!   - `BulkUpdateChannelOrdering` (`biz/channel_bulk.go:22`): per-item
//!     `SetOrderingWeight`, first failure aborts.
//!   - API-key management (`biz/channel_apikey.go`): `DisableAPIKey` (resolver
//!     passes the fixed `(0, "Manually disabled by user")` pair; disables the
//!     whole channel when no enabled key remains), `EnableAPIKey`,
//!     `EnableAllAPIKeys`, `EnableSelectedAPIKeys`, `DeleteDisabledAPIKeys`
//!     (OAuth channels rejected; last key preserved with `ONE_KEY_PRESERVED`).
//!   - `SyncChannelModels` (`biz/channel_model_sync.go:142`) and the
//!     `TestChannelOrchestrator` probes require a live upstream HTTP call —
//!     DEFER'd (see the method bodies).

use std::sync::Arc;

use async_graphql::ID;
use async_trait::async_trait;
use serde_json::{Map as JsonMap, Value};
use sqlx::PgPool;

use crate::conv::channel_row_to_gql;
use conduit_admin_graphql::channel::{
    CapabilityPolicy, Channel as GqlChannel, ChannelCredentialsInput, ChannelEndpointInput,
    ChannelPoliciesInput, ChannelServiceError, ChannelSettingsInput, ChannelStatus, ChannelType,
    CreateChannelInput, OAuthCredentialsInput, OverrideOperationInput, ProxyConfigInput, ProxyType,
};
use conduit_admin_graphql::channel_ext::{
    ChannelExtMutationServices, SaveChannelEndpointsInput, SyncChannelModelsPayload,
    TestApiKeyResult, TestChannelApiKeysPayload, TestChannelInput, TestChannelPayload,
};
use conduit_admin_graphql::channel_ext2::{
    BulkCreateChannelsInput, BulkImportChannelItem, BulkImportChannelsResult,
    ChannelBulkMutationServices, ChannelOrderingItem, DeleteDisabledApiKeysPayload,
};
use conduit_core::objects::channel_settings as core_ch;
use conduit_core::objects::overrides as core_ov;
use conduit_db::repo::channel_repo::{
    CreateChannelInput as RepoCreateChannelInput, ListChannelsQuery,
    UpdateChannelInput as RepoUpdateChannelInput,
};
use conduit_db::row::ChannelRow;
use conduit_db::{ChannelRepo, PolicyContext, Principal, RepoError, RequestContext};

/// Fixed disable-reason pair the Go resolver passes to
/// `ChannelService.DisableAPIKey` (conduit.resolvers.go:349).
const MANUAL_DISABLE_REASON: &str = "Manually disabled by user";
const MANUAL_DISABLE_ERROR_CODE: i64 = 0;

/// Max concurrency when probing every API key of a channel (Go
/// `testChannelAPIKeysMaxConcurrency`, tester.go:27).
const TEST_CHANNEL_API_KEYS_MAX_CONCURRENCY: usize = 8;
const CHANNEL_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const CHANNEL_PROBE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn channel_probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CHANNEL_PROBE_CONNECT_TIMEOUT)
        .timeout(CHANNEL_PROBE_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn credential_preflight_error(channel_type: &str, base_url: &str, key: &str) -> Option<String> {
    if channel_type == "openai"
        && base_url
            .trim_end_matches('/')
            .eq_ignore_ascii_case("https://api.openai.com/v1")
        && (!key.starts_with("sk-") || key.len() < 20)
    {
        return Some(
            "The configured key is not a usable OpenAI API key. Replace the placeholder/short key in channel settings before testing."
                .to_string(),
        );
    }
    None
}

/// One channel-test probe against a single upstream `(base_url, api_key)`
/// using the fixed request the Go `TestChannelOrchestrator` sends (system +
/// two-part user message, `max_completion_tokens = 256`, non-stream). Mirrors
/// the observable contract of Go `tester.go` `TestChannel` / `testSingleKey`:
/// returns `(latency_seconds, success, message, error)`.
///
/// Scope note: only OpenAI-compatible channel types are probed here (Bearer
/// auth, `{base_url}/chat/completions`). That covers the large majority of
/// channel types (openai/deepseek/doubao/moonshot/openrouter/xai/… — the same
/// set the host's `TransformerRegistry` maps to the OpenAI-compat outbound).
/// Anthropic/Gemini use different request shapes + auth headers; probing them
/// would require driving their outbound transformers, so they return a
/// descriptive "not supported" error rather than a fabricated result.
async fn probe_openai_compat_channel(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> (f64, bool, Option<String>, Option<String>) {
    // Fixed test payload (tester.go:126-153). User content is the two-part
    // text array Go sends; system is a plain string.
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": "You are a helpful assistant." },
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "Hello world, I'm Conduit API." },
                    { "type": "text", "text": "Please tell me who you are?" }
                ]
            }
        ],
        "max_completion_tokens": 256,
        "stream": false
    });

    // `{base_url}/chat/completions` (base_url already carries the `/v1`
    // segment for OpenAI-compatible providers, e.g. `https://host/v1`).
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let started = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await;

    let elapsed = || started.elapsed().as_secs_f64();

    let resp = match resp {
        Ok(r) => r,
        Err(err) => {
            // Transport error (DNS/TLS/connect/timeout) — mirrors Go's
            // `inbound.TransformError` surfacing the transport failure.
            return (elapsed(), false, None, Some(err.to_string()));
        }
    };

    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(err) => return (elapsed(), false, None, Some(err.to_string())),
    };
    let latency = elapsed();

    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

    if !status.is_success() {
        // Extract `error.message` like Go `gjson.GetBytes(body,
        // "error.message")`; fall back to the raw body / status line.
        let message = json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("upstream returned status {}", status.as_u16())
                } else {
                    text.clone()
                }
            });
        return (latency, false, None, Some(message));
    }

    // Success path: pull `choices[0].message.content` (tester.go:199-213).
    let choices = json.get("choices").and_then(|c| c.as_array());
    match choices {
        Some(list) if !list.is_empty() => {
            let content = list[0]
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(str::to_string);
            (latency, true, content, None)
        }
        _ => (
            latency,
            false,
            None,
            Some("No message in response".to_string()),
        ),
    }
}

/// Probe an Anthropic-native channel via `POST {base}/v1/messages`.
///
/// Mirrors the Go tester's channel-agnostic pipeline path for Anthropic
/// channels: the same fixed test prompt, but shaped into the native Messages
/// wire form (top-level `system` string, `x-api-key` + `anthropic-version`
/// auth) and the response content pulled from `content[].text`.
async fn probe_anthropic_channel(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> (f64, bool, Option<String>, Option<String>) {
    // Anthropic has no `system` role inside `messages`; the system prompt is a
    // top-level field. User content mirrors the OpenAI-compat two-part array.
    let body = serde_json::json!({
        "model": model,
        "system": "You are a helpful assistant.",
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": "Hello world, I'm Conduit API." },
                    { "type": "text", "text": "Please tell me who you are?" }
                ]
            }
        ],
        "max_tokens": 256,
        "stream": false
    });

    // Endpoint resolution mirrors `model_fetch`'s Anthropic base handling:
    // trim a trailing `/anthropic` or `/claude`, then append `/messages` when
    // the base already carries `/v1`, else `/v1/messages`.
    let trimmed = base_url
        .trim_end_matches('/')
        .trim_end_matches("/anthropic")
        .trim_end_matches("/claude")
        .trim_end_matches('/');
    let url = if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    };

    let started = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await;

    let elapsed = || started.elapsed().as_secs_f64();

    let resp = match resp {
        Ok(r) => r,
        Err(err) => return (elapsed(), false, None, Some(err.to_string())),
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(err) => return (elapsed(), false, None, Some(err.to_string())),
    };
    let latency = elapsed();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

    if !status.is_success() {
        let message = json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("upstream returned status {}", status.as_u16())
                } else {
                    text.clone()
                }
            });
        return (latency, false, None, Some(message));
    }

    // Success path: native Anthropic responses carry `content: [{type,text}]`.
    let content = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|list| {
            list.iter()
                .find_map(|block| block.get("text").and_then(|t| t.as_str()))
        })
        .map(str::to_string);
    match content {
        Some(text) => (latency, true, Some(text), None),
        None => (
            latency,
            false,
            None,
            Some("No message in response".to_string()),
        ),
    }
}

/// Probe a Gemini-native channel via
/// `POST {base}/v1beta/models/{model}:generateContent`.
///
/// Mirrors the Go tester path for Gemini channels: the fixed test prompt
/// shaped into `contents` + `systemInstruction`, `x-goog-api-key` auth, and
/// response text pulled from `candidates[0].content.parts[].text`.
async fn probe_gemini_channel(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> (f64, bool, Option<String>, Option<String>) {
    let body = serde_json::json!({
        "systemInstruction": {
            "parts": [ { "text": "You are a helpful assistant." } ]
        },
        "contents": [
            {
                "role": "user",
                "parts": [
                    { "text": "Hello world, I'm Conduit API." },
                    { "text": "Please tell me who you are?" }
                ]
            }
        ],
        "generationConfig": { "maxOutputTokens": 256 }
    });

    // `{base}/models/{model}:generateContent` when the base already carries a
    // `/v1` segment, else `{base}/v1beta/models/{model}:generateContent`
    // (mirrors `model_fetch`'s Gemini endpoint resolution).
    let trimmed = base_url.trim_end_matches('/');
    let url = if trimmed.contains("/v1") {
        format!("{trimmed}/models/{model}:generateContent")
    } else {
        format!("{trimmed}/v1beta/models/{model}:generateContent")
    };

    let started = std::time::Instant::now();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await;

    let elapsed = || started.elapsed().as_secs_f64();

    let resp = match resp {
        Ok(r) => r,
        Err(err) => return (elapsed(), false, None, Some(err.to_string())),
    };
    let status = resp.status();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(err) => return (elapsed(), false, None, Some(err.to_string())),
    };
    let latency = elapsed();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);

    if !status.is_success() {
        let message = json
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("upstream returned status {}", status.as_u16())
                } else {
                    text.clone()
                }
            });
        return (latency, false, None, Some(message));
    }

    // Success path: `candidates[0].content.parts[].text`.
    let content = json
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|list| list.first())
        .and_then(|cand| cand.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|part| part.get("text").and_then(|t| t.as_str()))
        })
        .map(str::to_string);
    match content {
        Some(text) => (latency, true, Some(text), None),
        None => (
            latency,
            false,
            None,
            Some("No message in response".to_string()),
        ),
    }
}

/// Dispatch a channel probe to the right per-type helper. All three channel
/// families (OpenAI-compatible, Anthropic-native, Gemini-native) are now
/// probeable, mirroring Go's channel-type-agnostic tester (which routes every
/// type through the pipeline's per-channel outbound transformer).
async fn probe_channel(
    client: &reqwest::Client,
    channel_type: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
) -> (f64, bool, Option<String>, Option<String>) {
    match channel_type {
        "anthropic" => probe_anthropic_channel(client, base_url, api_key, model).await,
        "gemini" => probe_gemini_channel(client, base_url, api_key, model).await,
        _ => probe_openai_compat_channel(client, base_url, api_key, model).await,
    }
}

/// Mask an API key for display — Go `maskAPIKey` (tester.go:598-604):
/// `≤8 chars → "****"`, else `first4 + "****" + last4`.
fn mask_api_key(key: &str) -> String {
    if key.chars().count() <= 8 {
        "****".to_string()
    } else {
        let chars: Vec<char> = key.chars().collect();
        let first: String = chars[..4].iter().collect();
        let last: String = chars[chars.len() - 4..].iter().collect();
        format!("{first}****{last}")
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing Channel extended/bulk mutation adapter backed by the live
/// [`ChannelRepo`] and [`ChannelModelPriceRepo`] implementations. Implements both
/// [`ChannelExtMutationServices`] and
/// [`ChannelBulkMutationServices`].
pub struct ChannelExtMutationAdapter {
    channel_repo: Arc<dyn ChannelRepo>,
    pool: PgPool,
}

impl ChannelExtMutationAdapter {
    pub fn new(channel_repo: Arc<dyn ChannelRepo>, pool: PgPool) -> Self {
        Self { channel_repo, pool }
    }

    /// Materialize every live (non-deleted) channel row (same bounded
    /// materialization strategy `ChannelCrudAdapter::load_all` uses — the
    /// channels table is small).
    async fn load_all(&self) -> Result<Vec<ChannelRow>, ChannelServiceError> {
        let ctx = boot_request_context();
        let mut rows = Vec::new();
        let mut offset = 0u32;
        const PAGE: u32 = 500;
        loop {
            let query = ListChannelsQuery {
                limit: PAGE,
                offset,
                after_created_at: None,
                after_id: None,
                status_in: Vec::new(),
            };
            let result = self
                .channel_repo
                .list_channels(&ctx, &query)
                .await
                .map_err(|e| ChannelServiceError::Query(e.to_string()))?;
            let fetched = result.rows.len();
            rows.extend(result.rows);
            if !result.has_more || fetched == 0 {
                break;
            }
            offset += PAGE;
        }
        Ok(rows)
    }

    /// Decode a raw GraphQL id and load the live row, mapping "missing" to the
    /// Go-equivalent `ent: channel not found`.
    async fn load_row(&self, raw_id: &str) -> Result<ChannelRow, ChannelServiceError> {
        let ctx = boot_request_context();
        let db_id = channel_db_id(raw_id).ok_or(ChannelServiceError::NotFound)?;
        self.channel_repo
            .find_channel(&ctx, &db_id)
            .await
            .map_err(|e| ChannelServiceError::Query(e.to_string()))?
            .ok_or(ChannelServiceError::NotFound)
    }

    /// Lower a GraphQL `CreateChannelInput` for the transactional duplicate
    /// path (mirrors `ChannelCrudAdapter::create_channel` / Go
    /// `biz.createChannel`).
    fn prepare_create_from_gql_input(
        input: CreateChannelInput,
    ) -> Result<(String, RepoCreateChannelInput), ChannelServiceError> {
        // Retained for the duplicate-name error (repo `NameConflict` maps to Go
        // `xerrors.DuplicateNameError("channel", …)`).
        let name = input.name.clone();
        let website_url =
            crate::wiring_channel_crud::normalize_website_url(input.website_url.clone())
                .map_err(ChannelServiceError::Create)?;
        let quota_currency = crate::wiring_channel_crud::normalize_quota_currency(
            input.quota_currency.clone(),
            true,
        )
        .map_err(ChannelServiceError::Create)?;
        let actual_quota_used =
            crate::wiring_channel_crud::normalize_quota_amount(input.actual_quota_used.clone())
                .map_err(ChannelServiceError::Create)?;
        let quota_remaining =
            crate::wiring_channel_crud::normalize_quota_amount(input.quota_remaining.clone())
                .map_err(ChannelServiceError::Create)?;
        let repo_input = RepoCreateChannelInput {
            // PostgreSQL owns the generated PK; `id` is ignored on
            // insert.
            id: String::new(),
            channel_type: channel_type_to_wire(input.channel_type).to_string(),
            name: input.name,
            base_url: input.base_url,
            website_url,
            quota_currency,
            actual_quota_used,
            quota_remaining,
            credentials: credentials_input_to_json(input.credentials),
            supported_models: input.supported_models,
            manual_models: input.manual_models.unwrap_or_default(),
            default_test_model: input.default_test_model,
            auto_sync_supported_models: input.auto_sync_supported_models.unwrap_or(false),
            auto_sync_model_pattern: input.auto_sync_model_pattern.unwrap_or_default(),
            tags: input.tags.unwrap_or_default(),
            policies: input.policies.map(policies_input_to_json),
            settings: input.settings.map(settings_input_to_json),
            endpoints: input
                .endpoints
                .map(|v| v.into_iter().map(endpoint_input_to_json).collect())
                .unwrap_or_default(),
            remark: input.remark,
            ordering_weight: input.ordering_weight.unwrap_or(0),
            created_at: now_rfc3339(),
        };
        Ok((name, repo_input))
    }

    /// Shared body of the bulk status mutations. Mirrors Go
    /// `bulkUpdateChannelStatus` (biz/channel_bulk.go:137): empty ids is a
    /// no-op; every id must exist BEFORE any row is touched; recover clears
    /// the error message alongside setting `enabled`.
    async fn bulk_update_status(
        &self,
        ids: Vec<ID>,
        status: &str,
        clear_error_message: bool,
    ) -> Result<(), ChannelServiceError> {
        if ids.is_empty() {
            return Ok(());
        }
        let ctx = boot_request_context();

        // Verify all channels exist first (Go counts `IDIn(ids)` and errors on
        // a mismatch before writing anything).
        let mut db_ids = Vec::with_capacity(ids.len());
        for id in &ids {
            let db_id = channel_db_id(id.as_str()).ok_or(ChannelServiceError::NotFound)?;
            let exists = self
                .channel_repo
                .find_channel(&ctx, &db_id)
                .await
                .map_err(|e| ChannelServiceError::Query(e.to_string()))?;
            if exists.is_none() {
                return Err(ChannelServiceError::NotFound);
            }
            db_ids.push(db_id);
        }

        let now = now_rfc3339();
        for db_id in &db_ids {
            let input = RepoUpdateChannelInput {
                status: Some(status.to_string()),
                error_message: if clear_error_message {
                    Some(None)
                } else {
                    None
                },
                updated_at: now.clone(),
                ..Default::default()
            };
            self.channel_repo
                .update_channel(&ctx, db_id, input)
                .await
                .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        }
        Ok(())
    }
}

/// The per-request context the host uses for repo calls (trusted, fully
/// authorized principal — the admin GraphQL layer performs its own auth before
/// reaching the service). Mirrors `wiring_channel_crud::boot_request_context`.
fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

/// Decode a GraphQL `ID!` (`gid://conduit/Channel/<n>` wire form or a bare
/// numeric id) into the numeric DB-id string the repo expects. Mirrors Go
/// `GUID.UnmarshalGQL`; anything else is treated as "no such row".
fn channel_db_id(raw: &str) -> Option<String> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// ChannelExtMutationServices — status / duplicate / endpoints / probes /
// bulk-status / sync
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelExtMutationServices for ChannelExtMutationAdapter {
    /// Go `UpdateChannelStatus` (biz/channel.go:771): a bare
    /// `UpdateOneID(id).SetStatus(status)`.
    async fn update_channel_status(
        &self,
        id: &str,
        status: ChannelStatus,
    ) -> Result<GqlChannel, ChannelServiceError> {
        let ctx = boot_request_context();
        let db_id = channel_db_id(id).ok_or(ChannelServiceError::NotFound)?;
        let input = RepoUpdateChannelInput {
            status: Some(channel_status_to_wire(status).to_string()),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        let row = self
            .channel_repo
            .update_channel(&ctx, &db_id, input)
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => ChannelServiceError::NotFound,
                other => ChannelServiceError::Update(other.to_string()),
            })?;
        Ok(channel_row_to_gql(row))
    }

    /// Go `DuplicateChannel` (biz/channel_duplicate.go:17): verify the source
    /// channel exists, then create from the provided input (the input carries
    /// the whole clone payload; the repo raises `NameConflict` for the
    /// duplicate-name check). Then copy the source channel's current model
    /// prices into a provider-price ChangeSet draft. Approval is the only path
    /// that can create the new channel's formal price heads and versions.
    async fn duplicate_channel(
        &self,
        actor_user_id: Option<i64>,
        source_id: &str,
        input: CreateChannelInput,
    ) -> Result<GqlChannel, ChannelServiceError> {
        let source_db = channel_db_id(source_id).ok_or(ChannelServiceError::NotFound)?;
        let (new_name, repo_input) = Self::prepare_create_from_gql_input(input)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ChannelServiceError::Create(e.to_string()))?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(|e| ChannelServiceError::Create(e.to_string()))?;
        let accounting_settings = crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(&mut tx)
            .await
            .map_err(|e| ChannelServiceError::Create(e.to_string()))?;

        // "failed to get source channel" — a missing source aborts before any
        // write, inside the same snapshot used for the price copy.
        let source = conduit_db::PgChannelRepo::find_channel_in_tx(&mut tx, &source_db)
            .await
            .map_err(|e| ChannelServiceError::Query(e.to_string()))?
            .ok_or(ChannelServiceError::NotFound)?;
        let source_db_id = source.id.parse::<i64>().map_err(|_| {
            ChannelServiceError::Query("source channel id is not an integer".to_string())
        })?;
        // Copy current model prices from the source channel (Go
        // `ChannelModelPrice.Query().Where(ChannelID(sourceID)).All`). Resolve
        // them before creating the destination channel and preserve each
        // row's currency: copying must never relabel a numeric amount.
        let now_at = chrono::Utc::now();
        let source_prices = conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(
            &mut tx,
            source_db_id,
        )
        .await
        .map_err(|e| {
            ChannelServiceError::Query(format!("failed to query source channel model prices: {e}"))
        })?;
        let row = conduit_db::PgChannelRepo::create_channel_in_tx(&mut tx, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => ChannelServiceError::DuplicateName(new_name),
                other => ChannelServiceError::Create(other.to_string()),
            })?;
        let new_db_id = row.id.parse::<i64>().map_err(|_| {
            ChannelServiceError::Create("created channel id is not an integer".to_string())
        })?;
        crate::wiring_postgres_change_sets::stage_duplicated_provider_prices(
            &mut tx,
            actor_user_id,
            source_db_id,
            new_db_id,
            &row.name,
            &source_prices,
            &accounting_settings.currency,
            accounting_settings.version,
            now_at,
        )
        .await
        .map_err(|e| ChannelServiceError::Create(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| ChannelServiceError::Create(e.to_string()))?;
        Ok(channel_row_to_gql(row))
    }

    /// Go `SaveChannelEndpoints` (biz/channel.go:800): validate, then
    /// `SetEndpoints` on the loaded channel.
    async fn save_channel_endpoints(
        &self,
        input: SaveChannelEndpointsInput,
    ) -> Result<GqlChannel, ChannelServiceError> {
        if let Err(msg) = validate_endpoints(&input.endpoints) {
            // Go wraps as "invalid endpoints: %w".
            return Err(ChannelServiceError::Update(format!(
                "invalid endpoints: {msg}"
            )));
        }
        // Load first — Go Gets the channel before updating ("failed to get
        // channel" on a missing id).
        let row = self.load_row(input.channel_id.as_str()).await?;
        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            endpoints: Some(
                input
                    .endpoints
                    .into_iter()
                    .map(endpoint_input_to_json)
                    .collect(),
            ),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        let row = self
            .channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        Ok(channel_row_to_gql(row))
    }

    /// Live upstream probe — Go `TestChannelOrchestrator.TestChannel`
    /// (tester.go:82-214). Loads the channel, picks the test model
    /// (`input.modelID` or the channel's `default_test_model`), takes the first
    /// enabled API key, and sends the fixed test chat request to the channel's
    /// endpoint. Returns latency + success + response message / error.
    ///
    /// Unlike the normal proxy path this targets a specific channel regardless
    /// of its enabled/disabled status (a channel is typically tested before it
    /// is enabled), mirroring Go's `SpecifiedChannelSelector`.
    async fn test_channel(
        &self,
        input: TestChannelInput,
    ) -> Result<TestChannelPayload, ChannelServiceError> {
        let row = self.load_row(input.channel_id.as_str()).await?;

        let base_url = match row.base_url.as_deref().filter(|s| !s.is_empty()) {
            Some(url) => url,
            None => {
                return Ok(TestChannelPayload {
                    latency: 0.0,
                    success: false,
                    message: None,
                    error: Some("channel has no base URL configured".to_string()),
                });
            }
        };

        let test_model = input
            .model_id
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| row.default_test_model.clone());

        // First enabled key (Go's load balancer picks one key; for a probe the
        // first enabled key is representative).
        let credentials = parse_credentials(&row);
        let disabled = parse_disabled_keys(&row);
        let key = credentials
            .get_enabled_api_keys(&disabled)
            .and_then(|keys| keys.into_iter().next());
        let key = match key {
            Some(k) => k,
            None => {
                return Ok(TestChannelPayload {
                    latency: 0.0,
                    success: false,
                    message: None,
                    error: Some("no enabled API keys configured for channel".to_string()),
                });
            }
        };

        if let Some(error) = credential_preflight_error(&row.channel_type, base_url, &key) {
            return Ok(TestChannelPayload {
                latency: 0.0,
                success: false,
                message: None,
                error: Some(error),
            });
        }
        let client = channel_probe_client();
        let (latency, success, message, error) =
            probe_channel(&client, &row.channel_type, base_url, &key, &test_model).await;

        Ok(TestChannelPayload {
            latency,
            success,
            message,
            error,
        })
    }

    /// Live upstream probe of one specific API key — Go
    /// `TestChannelOrchestrator.TestSingleAPIKey` (tester.go:411-451). Verifies
    /// the key actually belongs to the channel before probing.
    async fn test_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
        model_id: Option<String>,
    ) -> Result<TestApiKeyResult, ChannelServiceError> {
        let row = self.load_row(channel_id).await?;
        let credentials = parse_credentials(&row);
        let all_keys = credentials.get_all_api_keys().unwrap_or_default();
        if all_keys.is_empty() {
            return Err(ChannelServiceError::Update(
                "no API keys configured for channel".to_string(),
            ));
        }
        if !all_keys.iter().any(|k| k == key) {
            return Err(ChannelServiceError::Update(
                "the provided API key is not configured for this channel".to_string(),
            ));
        }

        let disabled = parse_disabled_keys(&row);
        let is_disabled = disabled.iter().any(|d| d.key == key);
        let key_prefix = mask_api_key(key);

        let base_url = match row.base_url.as_deref().filter(|s| !s.is_empty()) {
            Some(url) => url,
            None => {
                return Ok(TestApiKeyResult {
                    key_prefix,
                    success: false,
                    latency: 0.0,
                    error: Some("channel has no base URL configured".to_string()),
                    disabled: is_disabled,
                });
            }
        };
        let test_model = model_id
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| row.default_test_model.clone());

        if let Some(error) = credential_preflight_error(&row.channel_type, base_url, key) {
            return Ok(TestApiKeyResult {
                key_prefix,
                success: false,
                latency: 0.0,
                error: Some(error),
                disabled: is_disabled,
            });
        }
        let client = channel_probe_client();
        let (latency, success, _message, error) =
            probe_channel(&client, &row.channel_type, base_url, key, &test_model).await;

        Ok(TestApiKeyResult {
            key_prefix,
            success,
            latency,
            error,
            disabled: is_disabled,
        })
    }

    /// Live upstream probe of every API key — Go
    /// `TestChannelOrchestrator.TestChannelAPIKeys` (tester.go:321-407). Probes
    /// each configured key (bounded concurrency) and aggregates the results.
    async fn test_channel_api_keys(
        &self,
        channel_id: &str,
        model_id: Option<String>,
    ) -> Result<TestChannelApiKeysPayload, ChannelServiceError> {
        let row = self.load_row(channel_id).await?;
        let gql_id = channel_row_to_gql(row.clone()).id;

        let credentials = parse_credentials(&row);
        let all_keys = credentials.get_all_api_keys().unwrap_or_default();
        if all_keys.is_empty() {
            return Err(ChannelServiceError::Update(
                "no API keys configured for channel".to_string(),
            ));
        }
        let disabled = parse_disabled_keys(&row);
        let disabled_set: std::collections::HashSet<&str> =
            disabled.iter().map(|d| d.key.as_str()).collect();
        let test_model = model_id
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| row.default_test_model.clone());
        let channel_type = row.channel_type.clone();
        let base_url = row.base_url.clone().unwrap_or_default();

        let client = channel_probe_client();

        // Bounded-concurrency fan-out (Go caps at 8, tester.go:357-358). Probe
        // each key on its own task, tag with the original index so the result
        // vector preserves input order regardless of completion order. A simple
        // semaphore bounds how many probes run at once.
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            TEST_CHANNEL_API_KEYS_MAX_CONCURRENCY.min(all_keys.len().max(1)),
        ));
        let mut join_set = tokio::task::JoinSet::new();
        for (index, key) in all_keys.into_iter().enumerate() {
            let client = client.clone();
            let base_url = base_url.clone();
            let test_model = test_model.clone();
            let channel_type = channel_type.clone();
            let is_disabled = disabled_set.contains(key.as_str());
            let semaphore = Arc::clone(&semaphore);
            join_set.spawn(async move {
                // Permit dropped at task end; Err only if the semaphore is
                // closed (never here), so fall back to running unbounded.
                let _permit = semaphore.acquire_owned().await.ok();
                let key_prefix = mask_api_key(&key);
                let result = if base_url.is_empty() {
                    TestApiKeyResult {
                        key_prefix,
                        success: false,
                        latency: 0.0,
                        error: Some("channel has no base URL configured".to_string()),
                        disabled: is_disabled,
                    }
                } else if let Some(error) =
                    credential_preflight_error(&channel_type, &base_url, &key)
                {
                    TestApiKeyResult {
                        key_prefix,
                        success: false,
                        latency: 0.0,
                        error: Some(error),
                        disabled: is_disabled,
                    }
                } else {
                    let (latency, success, _msg, error) =
                        probe_channel(&client, &channel_type, &base_url, &key, &test_model).await;
                    TestApiKeyResult {
                        key_prefix,
                        success,
                        latency,
                        error,
                        disabled: is_disabled,
                    }
                };
                (index, result)
            });
        }

        let mut indexed: Vec<(usize, TestApiKeyResult)> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok(pair) => indexed.push(pair),
                // A panicked probe task should not abort the whole aggregate;
                // it simply contributes no result (Go's errgroup returns nil
                // per-key and records failures in-band).
                Err(_) => {}
            }
        }
        indexed.sort_by_key(|(i, _)| *i);
        let results: Vec<TestApiKeyResult> = indexed.into_iter().map(|(_, r)| r).collect();

        let total = i32::try_from(results.len()).unwrap_or(i32::MAX);
        let success_count =
            i32::try_from(results.iter().filter(|r| r.success).count()).unwrap_or(i32::MAX);
        let failed_count = total - success_count;

        Ok(TestChannelApiKeysPayload {
            channel_id: gql_id,
            total,
            success_count,
            failed_count,
            results,
        })
    }

    /// Go `BulkArchiveChannels` (biz/channel_bulk.go:174).
    async fn bulk_archive_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
        self.bulk_update_status(ids, "archived", false).await
    }

    /// Go `BulkDisableChannels` (biz/channel_bulk.go:179).
    async fn bulk_disable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
        self.bulk_update_status(ids, "disabled", false).await
    }

    /// Go `BulkEnableChannels` (biz/channel_bulk.go:184).
    async fn bulk_enable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
        self.bulk_update_status(ids, "enabled", false).await
    }

    /// Go `BulkRecoverChannels` (biz/channel_bulk.go:189): enabled + clear
    /// error message (does NOT touch `deleted_at`).
    async fn bulk_recover_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
        self.bulk_update_status(ids, "enabled", true).await
    }

    /// Go `BulkDeleteChannels` (biz/channel_bulk.go:194):
    /// `Delete().Where(IDIn(ids))` with the `SoftDeleteMixin` — a soft delete
    /// of whatever matches, with NO existence check (missing ids are skipped).
    async fn bulk_delete_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
        if ids.is_empty() {
            return Ok(());
        }
        let ctx = boot_request_context();
        let now = now_rfc3339();
        for id in &ids {
            // Undecodable ids error at the Go resolver's GUID decode.
            let db_id = channel_db_id(id.as_str()).ok_or(ChannelServiceError::NotFound)?;
            match self
                .channel_repo
                .soft_delete_channel(&ctx, &db_id, &now)
                .await
            {
                Ok(_) => {}
                // `IDIn` semantics: rows that do not exist are simply not
                // matched by the bulk delete.
                Err(RepoError::NotFound(_)) => {}
                Err(other) => return Err(ChannelServiceError::Delete(other.to_string())),
            }
        }
        Ok(())
    }

    /// DEFER: Go `SyncChannelModels` (biz/channel_model_sync.go:142) fetches
    /// the live model list from the upstream provider before merging — that
    /// upstream fetch is not wired into the host yet. The channel lookup is
    /// Go `SyncChannelModels` (biz/channel_model_sync.go:142 +
    /// `syncChannelModelsForChannel`): fetch the provider's model list, filter
    /// by the channel's `auto_sync_model_pattern` (or the override), merge with
    /// the channel's `manual_models`, and persist the result as
    /// `supported_models` (manual models preserved). Returns the merged list.
    async fn sync_channel_models(
        &self,
        channel_id: &str,
        pattern: Option<String>,
    ) -> Result<SyncChannelModelsPayload, ChannelServiceError> {
        let row = self.load_row(channel_id).await?;
        let gql_id = channel_row_to_gql(row.clone()).id;

        let base_url = row.base_url.clone().unwrap_or_default();
        // First enabled API key (fetch uses one key; OAuth-only channels have
        // none — mirror Go's "API key required" soft error).
        let credentials = parse_credentials(&row);
        let disabled = parse_disabled_keys(&row);
        let api_key = credentials
            .get_enabled_api_keys(&disabled)
            .and_then(|k| k.into_iter().next())
            .unwrap_or_default();

        let client = reqwest::Client::new();
        let fetched =
            crate::model_fetch::fetch_models(&client, &row.channel_type, &base_url, &api_key).await;
        if let Some(err) = fetched.error {
            return Err(ChannelServiceError::Update(format!(
                "failed to fetch models: {err}"
            )));
        }
        let mut fetched_ids = fetched.model_ids;

        // Filter by auto_sync_model_pattern (override wins), mirroring Go's
        // regex filter. An invalid regex is skipped (Go logs + continues).
        let effective_pattern = pattern.unwrap_or_else(|| row.auto_sync_model_pattern.clone());
        if !effective_pattern.is_empty()
            && let Ok(re) = regex::Regex::new(&effective_pattern)
        {
            fetched_ids.retain(|m| re.is_match(m));
        }

        // Merge manual models + fetched, de-duplicated preserving order
        // (manual first — Go `lo.Uniq(append(manualModels, fetchedModelIDs))`).
        let mut merged: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for m in row.manual_models.iter().chain(fetched_ids.iter()) {
            if seen.insert(m.clone()) {
                merged.push(m.clone());
            }
        }

        // Empty merge → keep existing (Go returns the channel unchanged).
        if merged.is_empty() {
            return Ok(SyncChannelModelsPayload {
                channel_id: gql_id,
                supported_models: row.supported_models.clone(),
            });
        }

        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            supported_models: Some(merged.clone()),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;

        Ok(SyncChannelModelsPayload {
            channel_id: gql_id,
            supported_models: merged,
        })
    }
}

// ---------------------------------------------------------------------------
// ChannelBulkMutationServices — bulk create / import / ordering + API keys
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelBulkMutationServices for ChannelExtMutationAdapter {
    /// Go `BulkCreateChannels` (biz/channel_bulk.go:61): fan out one channel
    /// per API key with numbered names, defaulting tags to `[name]` and
    /// credentials to `{apiKeys: [key]}`.
    async fn bulk_create_channels(
        &self,
        input: BulkCreateChannelsInput,
    ) -> Result<Vec<GqlChannel>, ChannelServiceError> {
        if input.api_keys.is_empty() {
            return Err(ChannelServiceError::Create("no API keys provided".into()));
        }
        let base_url = input
            .base_url
            .clone()
            .ok_or_else(|| ChannelServiceError::Create("base URL is required".into()))?;

        // Existing live names for the conflict-avoiding numbering (Go queries
        // all non-deleted channel names).
        let mut existing_names: std::collections::HashSet<String> =
            self.load_all().await?.into_iter().map(|r| r.name).collect();

        let tags_to_use = match &input.tags {
            Some(tags) if !tags.is_empty() => tags.clone(),
            // Base name as tag (Go backward-compat default).
            _ => vec![input.name.clone()],
        };

        let ctx = boot_request_context();
        let mut created = Vec::with_capacity(input.api_keys.len());
        let mut counter: u32 = 1;
        for api_key in &input.api_keys {
            // "base - (1)", "base - (2)", … skipping names already taken.
            let mut channel_name = format!("{} - ({})", input.name, counter);
            while existing_names.contains(&channel_name) {
                counter += 1;
                channel_name = format!("{} - ({})", input.name, counter);
            }
            counter += 1;
            existing_names.insert(channel_name.clone());

            let credentials = core_ch::ChannelCredentials {
                api_keys: vec![api_key.clone()],
                ..Default::default()
            };
            let repo_input = RepoCreateChannelInput {
                id: String::new(),
                channel_type: channel_type_to_wire(input.channel_type).to_string(),
                name: channel_name.clone(),
                base_url: Some(base_url.clone()),
                website_url: None,
                quota_currency: Some("USD".to_string()),
                actual_quota_used: None,
                quota_remaining: None,
                credentials: serde_json::to_value(credentials)
                    .unwrap_or_else(|_| Value::Object(JsonMap::new())),
                supported_models: input.supported_models.clone(),
                manual_models: Vec::new(),
                default_test_model: input.default_test_model.clone(),
                auto_sync_supported_models: input.auto_sync_supported_models.unwrap_or(false),
                auto_sync_model_pattern: String::new(),
                tags: tags_to_use.clone(),
                policies: input.policies.clone().map(policies_input_to_json),
                settings: input.settings.clone().map(settings_input_to_json),
                endpoints: Vec::new(),
                remark: input.remark.clone(),
                ordering_weight: input.ordering_weight.unwrap_or(0),
                created_at: now_rfc3339(),
            };
            let row = self
                .channel_repo
                .create_channel(&ctx, repo_input)
                .await
                .map_err(|e| {
                    ChannelServiceError::Create(format!(
                        "failed to create channel '{channel_name}': {e}"
                    ))
                })?;
            created.push(channel_row_to_gql(row));
        }
        Ok(created)
    }

    /// Go `BulkImportChannels` (biz/channel_bulk.go:234): best-effort — a bad
    /// row is recorded as a `"Row N …"` error string and skipped, never
    /// failing the whole mutation.
    async fn bulk_import_channels(
        &self,
        channels: Vec<BulkImportChannelItem>,
    ) -> Result<BulkImportChannelsResult, ChannelServiceError> {
        let ctx = boot_request_context();
        let mut created_channels: Vec<GqlChannel> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut created: i32 = 0;
        let mut failed: i32 = 0;

        for (i, item) in channels.into_iter().enumerate() {
            let row_no = i + 1;

            // channel.TypeValidator — the wire-literal whitelist.
            if !is_valid_channel_type(&item.channel_type) {
                errors.push(format!(
                    "Row {row_no}: Invalid channel type '{}'",
                    item.channel_type
                ));
                failed += 1;
                continue;
            }
            let base_url = match item.base_url.as_deref() {
                Some(url) if !url.is_empty() => url.to_string(),
                _ => {
                    errors.push(format!(
                        "Row {row_no} ({}): Base URL is required",
                        item.name
                    ));
                    failed += 1;
                    continue;
                }
            };
            let api_key = match item.api_key.as_deref() {
                Some(key) if !key.is_empty() => key.to_string(),
                _ => {
                    errors.push(format!("Row {row_no} ({}): API Key is required", item.name));
                    failed += 1;
                    continue;
                }
            };

            // Go sets only type/name/baseURL/credentials/supportedModels/
            // defaultTestModel; everything else takes the ent column defaults.
            let credentials = core_ch::ChannelCredentials {
                api_key,
                ..Default::default()
            };
            let repo_input = RepoCreateChannelInput {
                id: String::new(),
                channel_type: item.channel_type.clone(),
                name: item.name.clone(),
                base_url: Some(base_url),
                website_url: None,
                quota_currency: Some("USD".to_string()),
                actual_quota_used: None,
                quota_remaining: None,
                credentials: serde_json::to_value(credentials)
                    .unwrap_or_else(|_| Value::Object(JsonMap::new())),
                supported_models: item.supported_models,
                manual_models: Vec::new(),
                default_test_model: item.default_test_model,
                auto_sync_supported_models: false,
                auto_sync_model_pattern: String::new(),
                tags: Vec::new(),
                policies: None,
                settings: None,
                endpoints: Vec::new(),
                remark: None,
                ordering_weight: 0,
                created_at: now_rfc3339(),
            };
            match self.channel_repo.create_channel(&ctx, repo_input).await {
                Ok(row) => {
                    created_channels.push(channel_row_to_gql(row));
                    created += 1;
                }
                Err(e) => {
                    errors.push(format!("Row {row_no} ({}): {e}", item.name));
                    failed += 1;
                }
            }
        }

        Ok(BulkImportChannelsResult {
            success: failed == 0,
            created,
            failed,
            errors: if errors.is_empty() {
                None
            } else {
                Some(errors)
            },
            channels: created_channels,
        })
    }

    /// Go `BulkUpdateChannelOrdering` (biz/channel_bulk.go:22): per-item
    /// `SetOrderingWeight`; the first failure aborts the whole mutation.
    async fn bulk_update_channel_ordering(
        &self,
        items: Vec<ChannelOrderingItem>,
    ) -> Result<Vec<GqlChannel>, ChannelServiceError> {
        let ctx = boot_request_context();
        let mut updated = Vec::with_capacity(items.len());
        for item in items {
            let raw = item.id.as_str();
            let db_id = channel_db_id(raw).ok_or_else(|| {
                ChannelServiceError::Update(format!(
                    "failed to update channel {raw}: {}",
                    ChannelServiceError::NotFound
                ))
            })?;
            let input = RepoUpdateChannelInput {
                ordering_weight: Some(item.ordering_weight),
                updated_at: now_rfc3339(),
                ..Default::default()
            };
            let row = self
                .channel_repo
                .update_channel(&ctx, &db_id, input)
                .await
                .map_err(|e| {
                    ChannelServiceError::Update(format!("failed to update channel {raw}: {e}"))
                })?;
            updated.push(channel_row_to_gql(row));
        }
        Ok(updated)
    }

    /// Go `DisableAPIKey` (biz/channel_apikey.go:18) with the resolver's fixed
    /// `(0, "Manually disabled by user")` pair: keys not in the credentials or
    /// already disabled are ignored; when no enabled key remains the whole
    /// channel is disabled with an explanatory error message.
    async fn disable_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
    ) -> Result<(), ChannelServiceError> {
        if key.is_empty() {
            return Err(ChannelServiceError::Update(
                "api key cannot be empty".into(),
            ));
        }
        let row = self.load_row(channel_id).await?;
        let credentials = parse_credentials(&row);

        // Key not present in the credentials → ignore (Go returns nil).
        let all_keys = credentials.get_all_api_keys().unwrap_or_default();
        if !all_keys.iter().any(|k| k == key) {
            return Ok(());
        }
        let mut disabled = parse_disabled_keys(&row);
        // Already disabled → ignore.
        if disabled.iter().any(|dk| dk.key == key) {
            return Ok(());
        }
        disabled.push(core_ch::DisabledAPIKey {
            key: key.to_string(),
            disabled_at: chrono::Utc::now(),
            error_code: MANUAL_DISABLE_ERROR_CODE,
            reason: MANUAL_DISABLE_REASON.to_string(),
        });

        // Nothing enabled left → disable the whole channel.
        let channel_disabled = credentials.get_enabled_api_keys(&disabled).is_none();
        let mut update = RepoUpdateChannelInput {
            disabled_api_keys: Some(disabled_keys_to_value(&disabled)),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        if channel_disabled {
            update.status = Some("disabled".to_string());
            update.error_message = Some(Some(format!(
                "All API keys disabled (last error: {MANUAL_DISABLE_ERROR_CODE})"
            )));
        }
        let ctx = boot_request_context();
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        Ok(())
    }

    /// Go `EnableAPIKey` (biz/channel_apikey.go:105): remove the key from
    /// `disabled_api_keys`; a key that is not disabled is ignored.
    async fn enable_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
    ) -> Result<(), ChannelServiceError> {
        let row = self.load_row(channel_id).await?;
        let disabled = parse_disabled_keys(&row);
        if disabled.is_empty() {
            return Ok(());
        }
        let new_disabled: Vec<core_ch::DisabledAPIKey> = disabled
            .iter()
            .filter(|dk| dk.key != key)
            .cloned()
            .collect();
        if new_disabled.len() == disabled.len() {
            // Key was not in the disabled list → ignore.
            return Ok(());
        }
        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            disabled_api_keys: Some(disabled_keys_to_value(&new_disabled)),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        Ok(())
    }

    /// Go `EnableAllAPIKeys` (biz/channel_apikey.go:149): clear
    /// `disabled_api_keys` entirely; a channel with nothing disabled is a
    /// no-op.
    async fn enable_all_channel_api_keys(
        &self,
        channel_id: &str,
    ) -> Result<(), ChannelServiceError> {
        let row = self.load_row(channel_id).await?;
        if parse_disabled_keys(&row).is_empty() {
            return Ok(());
        }
        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            disabled_api_keys: Some(Value::Array(Vec::new())),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        Ok(())
    }

    /// Go `EnableSelectedAPIKeys` (biz/channel_apikey.go:179): remove only the
    /// listed keys from `disabled_api_keys`; no-op when nothing changes.
    async fn enable_selected_channel_api_keys(
        &self,
        channel_id: &str,
        keys: Vec<String>,
    ) -> Result<(), ChannelServiceError> {
        if keys.is_empty() {
            return Ok(());
        }
        let row = self.load_row(channel_id).await?;
        let disabled = parse_disabled_keys(&row);
        if disabled.is_empty() {
            return Ok(());
        }
        let to_enable: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
        let new_disabled: Vec<core_ch::DisabledAPIKey> = disabled
            .iter()
            .filter(|dk| !to_enable.contains(dk.key.as_str()))
            .cloned()
            .collect();
        if new_disabled.len() == disabled.len() {
            return Ok(());
        }
        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            disabled_api_keys: Some(disabled_keys_to_value(&new_disabled)),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;
        Ok(())
    }

    /// Go `DeleteDisabledAPIKeys` (biz/channel_apikey.go:234): remove the
    /// listed keys from both `disabled_api_keys` and the credentials; OAuth
    /// channels are rejected; at least one key is always preserved (message
    /// `ONE_KEY_PRESERVED`).
    async fn delete_disabled_channel_api_keys(
        &self,
        channel_id: &str,
        keys: Vec<String>,
    ) -> Result<DeleteDisabledApiKeysPayload, ChannelServiceError> {
        if keys.is_empty() {
            return Ok(DeleteDisabledApiKeysPayload {
                success: true,
                message: None,
            });
        }
        let row = self.load_row(channel_id).await?;
        let mut credentials = parse_credentials(&row);
        if credentials.is_oauth() {
            return Err(ChannelServiceError::Update(
                "cannot delete API keys for OAuth channels".into(),
            ));
        }

        let to_delete: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();

        // Remove from disabled_api_keys.
        let new_disabled: Vec<core_ch::DisabledAPIKey> = parse_disabled_keys(&row)
            .into_iter()
            .filter(|dk| !to_delete.contains(dk.key.as_str()))
            .collect();

        // Remove from credentials (both the list and the legacy single key).
        credentials
            .api_keys
            .retain(|k| !to_delete.contains(k.as_str()));
        if !credentials.api_key.is_empty() && to_delete.contains(credentials.api_key.as_str()) {
            credentials.api_key.clear();
        }

        // Ensure at least one API key remains — restore the first deleted key.
        let mut message = None;
        if credentials.get_all_api_keys().is_none() {
            if let Some(restored) = keys.first() {
                credentials.api_keys = vec![restored.clone()];
            }
            message = Some("ONE_KEY_PRESERVED".to_string());
        }

        let ctx = boot_request_context();
        let update = RepoUpdateChannelInput {
            credentials: Some(
                serde_json::to_value(&credentials)
                    .unwrap_or_else(|_| Value::Object(JsonMap::new())),
            ),
            disabled_api_keys: Some(disabled_keys_to_value(&new_disabled)),
            updated_at: now_rfc3339(),
            ..Default::default()
        };
        self.channel_repo
            .update_channel(&ctx, &row.id, update)
            .await
            .map_err(|e| ChannelServiceError::Update(e.to_string()))?;

        Ok(DeleteDisabledApiKeysPayload {
            success: true,
            message,
        })
    }
}

// ---------------------------------------------------------------------------
// Credentials / disabled-key JSON helpers (row columns are raw `Value`s)
// ---------------------------------------------------------------------------

/// Decode the `credentials` JSON column via the core typed object (Go
/// `objects.ChannelCredentials` serde parity); malformed data degrades to the
/// zero value, mirroring Ent's value-type unmarshalling.
fn parse_credentials(row: &ChannelRow) -> core_ch::ChannelCredentials {
    serde_json::from_value(row.credentials.clone()).unwrap_or_default()
}

/// Decode the `disabled_api_keys` JSON column (Go `[]objects.DisabledAPIKey`).
fn parse_disabled_keys(row: &ChannelRow) -> Vec<core_ch::DisabledAPIKey> {
    serde_json::from_value(row.disabled_api_keys.clone()).unwrap_or_default()
}

/// Serialize a disabled-key list back into the column value.
fn disabled_keys_to_value(keys: &[core_ch::DisabledAPIKey]) -> Value {
    serde_json::to_value(keys).unwrap_or_else(|_| Value::Array(Vec::new()))
}

// ---------------------------------------------------------------------------
// Endpoint validation — structural subset of Go `ValidateEndpoints`
// (biz/channel_endpoint.go:37)
// ---------------------------------------------------------------------------

/// Structural endpoint checks ported from Go: api_format required, no
/// duplicate api_format, path must be a rooted path (not a full URL).
///
/// DEFER: the `SupportedAPIFormats` whitelist and the websocket-transport
/// restriction reference the Go `llm` API-format constants, which have no Rust
/// port yet — copying the list by hand would risk synthesizing it, so those
/// two checks are deferred until the constants are ported.
fn validate_endpoints(endpoints: &[ChannelEndpointInput]) -> Result<(), String> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (i, ep) in endpoints.iter().enumerate() {
        if ep.api_format.is_empty() {
            return Err(format!("endpoint[{i}]: api_format is required"));
        }
        if !seen.insert(ep.api_format.as_str()) {
            return Err(format!(
                "endpoint[{i}]: duplicate api_format {:?}",
                ep.api_format
            ));
        }
        if let Some(path) = ep.path.as_deref()
            && !path.is_empty()
        {
            if path.starts_with("http://") || path.starts_with("https://") {
                return Err(format!(
                    "endpoint[{i}]: path must not be a full URL, got {path:?}"
                ));
            }
            if !path.starts_with('/') {
                return Err(format!(
                    "endpoint[{i}]: path must start with '/', got {path:?}"
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Enum → wire-literal maps + input lowering.
// Duplicated from `wiring_channel_crud.rs` (private there; single-file
// constraint) — keep the two copies in sync.
// ---------------------------------------------------------------------------

/// GraphQL `ChannelType` → the snake_case wire literal stored in the `type`
/// column. Exact reverse of `conv::channel_type_from_str`.
fn channel_type_to_wire(t: ChannelType) -> &'static str {
    match t {
        ChannelType::Openai => "openai",
        ChannelType::OpenaiResponses => "openai_responses",
        ChannelType::Atlascloud => "atlascloud",
        ChannelType::Codex => "codex",
        ChannelType::Vercel => "vercel",
        ChannelType::Anthropic => "anthropic",
        ChannelType::AnthropicAws => "anthropic_aws",
        ChannelType::AnthropicGcp => "anthropic_gcp",
        ChannelType::GeminiOpenai => "gemini_openai",
        ChannelType::Gemini => "gemini",
        ChannelType::GeminiVertex => "gemini_vertex",
        ChannelType::Deepseek => "deepseek",
        ChannelType::DeepseekAnthropic => "deepseek_anthropic",
        ChannelType::Deepinfra => "deepinfra",
        ChannelType::Qiniu => "qiniu",
        ChannelType::Fireworks => "fireworks",
        ChannelType::Doubao => "doubao",
        ChannelType::DoubaoAnthropic => "doubao_anthropic",
        ChannelType::Moonshot => "moonshot",
        ChannelType::MoonshotAnthropic => "moonshot_anthropic",
        ChannelType::Zhipu => "zhipu",
        ChannelType::Zai => "zai",
        ChannelType::ZhipuAnthropic => "zhipu_anthropic",
        ChannelType::ZaiAnthropic => "zai_anthropic",
        ChannelType::AnthropicFake => "anthropic_fake",
        ChannelType::OpenaiFake => "openai_fake",
        ChannelType::Openrouter => "openrouter",
        ChannelType::Xiaomi => "xiaomi",
        ChannelType::XiaomiAnthropic => "xiaomi_anthropic",
        ChannelType::Xai => "xai",
        ChannelType::Ppio => "ppio",
        ChannelType::Siliconflow => "siliconflow",
        ChannelType::Volcengine => "volcengine",
        ChannelType::VolcengineAnthropic => "volcengine_anthropic",
        ChannelType::Longcat => "longcat",
        ChannelType::LongcatAnthropic => "longcat_anthropic",
        ChannelType::Minimax => "minimax",
        ChannelType::MinimaxAnthropic => "minimax_anthropic",
        ChannelType::Aihubmix => "aihubmix",
        ChannelType::AihubmixAnthropic => "aihubmix_anthropic",
        ChannelType::Burncloud => "burncloud",
        ChannelType::Modelscope => "modelscope",
        ChannelType::Bailian => "bailian",
        ChannelType::BailianAnthropic => "bailian_anthropic",
        ChannelType::MoonshotCoding => "moonshot_coding",
        ChannelType::Jina => "jina",
        ChannelType::Github => "github",
        ChannelType::GithubCopilot => "github_copilot",
        ChannelType::Claudecode => "claudecode",
        ChannelType::Cerebras => "cerebras",
        ChannelType::Antigravity => "antigravity",
        ChannelType::Nanogpt => "nanogpt",
        ChannelType::NanogptResponses => "nanogpt_responses",
        ChannelType::OpencodeGo => "opencode_go",
        ChannelType::OpencodeGoAnthropic => "opencode_go_anthropic",
        ChannelType::Ollama => "ollama",
        ChannelType::Evolink => "evolink",
        ChannelType::EvolinkAnthropic => "evolink_anthropic",
    }
}

/// Whether a raw import `type` string is a valid channel type (Go
/// `channel.TypeValidator`). Same literal set as [`channel_type_to_wire`].
fn is_valid_channel_type(s: &str) -> bool {
    matches!(
        s,
        "openai"
            | "openai_responses"
            | "atlascloud"
            | "codex"
            | "vercel"
            | "anthropic"
            | "anthropic_aws"
            | "anthropic_gcp"
            | "gemini_openai"
            | "gemini"
            | "gemini_vertex"
            | "deepseek"
            | "deepseek_anthropic"
            | "deepinfra"
            | "qiniu"
            | "fireworks"
            | "doubao"
            | "doubao_anthropic"
            | "moonshot"
            | "moonshot_anthropic"
            | "zhipu"
            | "zai"
            | "zhipu_anthropic"
            | "zai_anthropic"
            | "anthropic_fake"
            | "openai_fake"
            | "openrouter"
            | "xiaomi"
            | "xiaomi_anthropic"
            | "xai"
            | "ppio"
            | "siliconflow"
            | "volcengine"
            | "volcengine_anthropic"
            | "longcat"
            | "longcat_anthropic"
            | "minimax"
            | "minimax_anthropic"
            | "aihubmix"
            | "aihubmix_anthropic"
            | "burncloud"
            | "modelscope"
            | "bailian"
            | "bailian_anthropic"
            | "moonshot_coding"
            | "jina"
            | "github"
            | "github_copilot"
            | "claudecode"
            | "cerebras"
            | "antigravity"
            | "nanogpt"
            | "nanogpt_responses"
            | "opencode_go"
            | "opencode_go_anthropic"
            | "ollama"
            | "evolink"
            | "evolink_anthropic"
    )
}

/// GraphQL `ChannelStatus` → the wire literal stored in the `status` column.
fn channel_status_to_wire(s: ChannelStatus) -> &'static str {
    match s {
        ChannelStatus::Enabled => "enabled",
        ChannelStatus::Disabled => "disabled",
        ChannelStatus::Archived => "archived",
    }
}

/// GraphQL `CapabilityPolicy` → the wire literal (`ChannelPolicies.stream`).
fn capability_to_wire(p: CapabilityPolicy) -> &'static str {
    match p {
        CapabilityPolicy::Unlimited => "unlimited",
        CapabilityPolicy::Require => "require",
        CapabilityPolicy::Forbid => "forbid",
    }
}

/// GraphQL `ProxyType` → the wire literal (`ProxyConfig.type`).
fn proxy_type_to_wire(p: ProxyType) -> &'static str {
    match p {
        ProxyType::Disabled => "DISABLED",
        ProxyType::Environment => "ENVIRONMENT",
        ProxyType::Url => "URL",
    }
}

fn policies_input_to_json(p: ChannelPoliciesInput) -> Value {
    let core = core_ch::ChannelPolicies {
        stream: p
            .stream
            .map(capability_to_wire)
            .map(str::to_string)
            .unwrap_or_default(),
    };
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(JsonMap::new()))
}

fn endpoint_input_to_json(e: ChannelEndpointInput) -> Value {
    let core = core_ch::ChannelEndpoint {
        api_format: e.api_format,
        path: e.path.unwrap_or_default(),
        base_url: e.base_url.unwrap_or_default(),
        transport: e.transport.unwrap_or_default(),
    };
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(JsonMap::new()))
}

fn settings_input_to_json(s: ChannelSettingsInput) -> Value {
    let core = core_ch::ChannelSettings {
        management_adapter: s
            .management_adapter
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty()),
        billing_currency: s
            .billing_currency
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase(),
        recharge_multiplier: s.recharge_multiplier.map(|value| value.0),
        extra_model_prefix: s.extra_model_prefix.unwrap_or_default(),
        auto_trimed_model_prefixes: s.auto_trimed_model_prefixes.unwrap_or_default(),
        model_mappings: s
            .model_mappings
            .unwrap_or_default()
            .into_iter()
            .map(|m| core_ch::ModelMapping {
                from: m.from,
                to: m.to,
            })
            .collect(),
        hide_original_models: s.hide_original_models.unwrap_or(false),
        hide_mapped_models: s.hide_mapped_models.unwrap_or(false),
        lowercase_model_id: s.lowercase_model_id.unwrap_or(false),
        override_parameters: String::new(),
        body_override_operations: s
            .body_override_operations
            .unwrap_or_default()
            .into_iter()
            .map(override_op_input_to_core)
            .collect(),
        override_headers: Vec::new(),
        header_override_operations: s
            .header_override_operations
            .unwrap_or_default()
            .into_iter()
            .map(override_op_input_to_core)
            .collect(),
        proxy: s.proxy.map(proxy_input_to_json),
        transform_options: s
            .transform_options
            .map(|t| core_ch::TransformOptions {
                force_array_instructions: t.force_array_instructions.unwrap_or(false),
                force_array_inputs: t.force_array_inputs.unwrap_or(false),
                replace_developer_role_with_system: t
                    .replace_developer_role_with_system
                    .unwrap_or(false),
            })
            .unwrap_or_default(),
        pass_through_user_agent: s.pass_through_user_agent,
        pass_through_body: s.pass_through_body,
        rate_limit: s.rate_limit.map(|r| core_ch::ChannelRateLimit {
            rpm: r.rpm,
            tpm: r.tpm,
            max_concurrent: r.max_concurrent,
            queue_size: r.queue_size,
            queue_timeout_ms: r.queue_timeout_ms,
        }),
        retryable_status_codes: s.retryable_status_codes.unwrap_or_default(),
        retryable_error_patterns: s
            .retryable_error_patterns
            .unwrap_or_default()
            .into_iter()
            .map(|p| core_ch::RetryableErrorPattern {
                pattern: p.pattern,
                regex: p.regex.unwrap_or(false),
            })
            .collect(),
        auto_model_mapping_rules: s
            .auto_model_mapping_rules
            .unwrap_or_default()
            .into_iter()
            .map(|rule| core_ch::AutoModelMappingRule {
                pattern: rule.pattern,
                public_model_id_template: rule.public_model_id_template,
                create_draft: rule.create_draft.unwrap_or(true),
                developer_template: rule.developer_template.unwrap_or_default(),
                name_template: rule.name_template.unwrap_or_default(),
                group_template: rule.group_template.unwrap_or_default(),
                model_type: rule.model_type.unwrap_or_else(|| "chat".to_string()),
            })
            .collect(),
        error_response_rewrite_rules: s
            .error_response_rewrite_rules
            .unwrap_or_default()
            .into_iter()
            .map(|rule| core_ch::ErrorResponseRewriteRule {
                status_codes: rule
                    .status_codes
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|status| u16::try_from(status).ok())
                    .collect(),
                body_pattern: rule.body_pattern.unwrap_or_default(),
                http_status: rule
                    .http_status
                    .and_then(|status| u16::try_from(status).ok()),
                message: rule.message,
                error_type: rule.error_type,
                code: rule.code,
                body: rule.body.map(|body| body.0),
            })
            .collect(),
    };
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(JsonMap::new()))
}

fn override_op_input_to_core(o: OverrideOperationInput) -> core_ov::OverrideOperation {
    core_ov::OverrideOperation {
        op: o.op,
        path: o.path.unwrap_or_default(),
        from: o.from.unwrap_or_default(),
        to: o.to.unwrap_or_default(),
        value: o.value.unwrap_or_default(),
        condition: o.condition.unwrap_or_default(),
        r#match: o.match_.map(|m| core_ov::OverrideMatch {
            path: m.path,
            eq: m.eq,
        }),
        index: o.index,
        splat: o.splat,
    }
}

/// Lower a `ProxyConfigInput` into the raw proxy JSON (Go `ProxyConfig` uses
/// single-word tags `type`/`url`/`username`/`password`).
fn proxy_input_to_json(p: ProxyConfigInput) -> Value {
    let mut obj = JsonMap::new();
    obj.insert(
        "type".to_string(),
        Value::String(proxy_type_to_wire(p.proxy_type).to_string()),
    );
    if let Some(url) = p.url {
        obj.insert("url".to_string(), Value::String(url));
    }
    if let Some(username) = p.username {
        obj.insert("username".to_string(), Value::String(username));
    }
    if let Some(password) = p.password {
        obj.insert("password".to_string(), Value::String(password));
    }
    Value::Object(obj)
}

/// Lower a `ChannelCredentialsInput` into the `credentials` JSON column value
/// (Go `objects.ChannelCredentials` parity: `apiKey`/`apiKeys`/`gcp`/`oauth`).
fn credentials_input_to_json(c: ChannelCredentialsInput) -> Value {
    let core = core_ch::ChannelCredentials {
        api_key: c.api_key.unwrap_or_default(),
        oauth: c.oauth.map(oauth_input_to_json),
        api_keys: c.api_keys.unwrap_or_default(),
        azure: None,
        gcp: c.gcp.map(|g| core_ch::GCPCredential {
            region: g.region,
            project_id: g.project_id,
            json_data: g.json_data,
        }),
    };
    serde_json::to_value(core).unwrap_or_else(|_| Value::Object(JsonMap::new()))
}

/// Lower an `OAuthCredentialsInput` into the raw oauth JSON (Go
/// `oauth.OAuthCredentials` snake_case tags).
fn oauth_input_to_json(o: OAuthCredentialsInput) -> Value {
    let mut obj = JsonMap::new();
    if let Some(client_id) = o.client_id {
        obj.insert("client_id".to_string(), Value::String(client_id));
    }
    obj.insert(
        "access_token".to_string(),
        Value::String(o.access_token.unwrap_or_default()),
    );
    obj.insert(
        "refresh_token".to_string(),
        Value::String(o.refresh_token.unwrap_or_default()),
    );
    if let Some(expires_at) = o.expires_at {
        obj.insert(
            "expires_at".to_string(),
            Value::String(expires_at.0.to_rfc3339()),
        );
    }
    if let Some(token_type) = o.token_type {
        obj.insert("token_type".to_string(), Value::String(token_type));
    }
    if let Some(scopes) = o.scopes {
        obj.insert(
            "scopes".to_string(),
            Value::Array(scopes.into_iter().map(Value::String).collect()),
        );
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::change_set::{
        ChangeSetKind, ChangeSetServices as _, ChangeSetStatus,
    };

    #[tokio::test]
    async fn postgres_duplicate_channel_stages_price_until_approval_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let source_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai','duplicate-price-source','enabled','{}'::jsonb,\
                    '[\"gpt-priced\"]'::jsonb,'gpt-priced','{}'::jsonb) RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let source_price = serde_json::json!({
            "items": [{
                "itemCode": "prompt_tokens",
                "pricing": {"mode": "usage_per_unit", "usagePerUnit": "1"}
            }]
        });
        sqlx::query(
            "INSERT INTO channel_model_prices \
             (channel_id,model_id,currency_code,price,reference_id) \
             VALUES($1,'gpt-priced','CNY',$2,'duplicate-source-price')",
        )
        .bind(source_id)
        .bind(sqlx::types::Json(source_price.clone()))
        .execute(&database.pool)
        .await?;

        let adapter = ChannelExtMutationAdapter::new(
            Arc::new(conduit_db::PgChannelRepo::new(database.pool.clone())),
            database.pool.clone(),
        );
        let duplicated = adapter
            .duplicate_channel(
                Some(77),
                &source_id.to_string(),
                CreateChannelInput {
                    channel_type: ChannelType::Openai,
                    base_url: None,
                    website_url: None,
                    quota_currency: None,
                    actual_quota_used: None,
                    quota_remaining: None,
                    name: "duplicate-price-target".into(),
                    credentials: ChannelCredentialsInput::default(),
                    supported_models: vec!["gpt-priced".into()],
                    manual_models: None,
                    auto_sync_supported_models: None,
                    auto_sync_model_pattern: None,
                    tags: None,
                    default_test_model: "gpt-priced".into(),
                    policies: None,
                    settings: None,
                    ordering_weight: None,
                    remark: None,
                    endpoints: None,
                },
            )
            .await?;
        let target_id = channel_db_id(duplicated.id.as_str())
            .ok_or("duplicated channel returned an invalid id")?
            .parse::<i64>()?;

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0"
            )
            .bind(target_id)
            .fetch_one(&database.pool)
            .await?,
            0,
            "duplicating a channel must not bypass ChangeSet approval"
        );

        let change_sets =
            crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(database.pool.clone());
        let drafts = change_sets
            .change_sets(
                Some(ChangeSetKind::ProviderPrice),
                Some(ChangeSetStatus::Draft),
                Some("channel".into()),
                Some(target_id.to_string()),
                10,
            )
            .await?;
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].items.len(), 1);
        assert_eq!(drafts[0].items[0].item_key, "gpt-priced");
        assert_eq!(
            drafts[0].items[0]
                .source_snapshot
                .as_ref()
                .and_then(|value| value.0.get("origin"))
                .and_then(Value::as_str),
            Some("channel_duplicate")
        );

        let submitted = change_sets
            .submit_change_set(77, drafts[0].id.clone())
            .await?;
        let applied = change_sets
            .approve_change_set(78, submitted.id, Some("copied price verified".into()))
            .await?;
        assert_eq!(applied.status, ChangeSetStatus::Applied);

        let copied_price = sqlx::query_as::<_, (i64, String, sqlx::types::Json<Value>)>(
            "SELECT id,currency_code,price FROM channel_model_prices \
             WHERE channel_id=$1 AND model_id='gpt-priced' AND deleted_at=0",
        )
        .bind(target_id)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(copied_price.1, "CNY");
        assert_eq!(copied_price.2.0, source_price);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_price_versions \
                 WHERE channel_id=$1 AND channel_model_price_id=$2 AND status='active' \
                   AND currency_code='CNY'",
            )
            .bind(target_id)
            .bind(copied_price.0)
            .fetch_one(&database.pool)
            .await?,
            1
        );

        let audit = sqlx::query_as::<
            _,
            (
                String,
                Option<i64>,
                String,
                String,
                String,
                i64,
                String,
                i64,
            ),
        >(
            "SELECT actor_type,actor_id,operation,entity_type,accounting_currency,\
                        accounting_settings_version,result,source_change_set_id \
                 FROM pricing_change_audits WHERE entity_id=$1",
        )
        .bind(format!("{target_id}:gpt-priced"))
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(
            audit,
            (
                "user".into(),
                Some(78),
                "apply_provider_price_change_set".into(),
                "channel_model_price".into(),
                "CNY".into(),
                1,
                "success".into(),
                applied.id.as_str().parse::<i64>()?,
            )
        );

        database.cleanup().await?;
        Ok(())
    }
}

//! ADPT-CHANNEL-CRUD — host adapter wiring the admin GraphQL Channel *base*
//! domain to the configured [`ChannelRepo`].
//!
//! Backs the two host-injected traits declared in
//! `crates/conduit-admin-graphql/src/channel.rs`:
//!   - [`ChannelQueryServices`]  — `Query.channels` connection
//!     (channel.rs:1306).
//!   - [`ChannelMutationServices`] — `createChannel` / `updateChannel` /
//!     `deleteChannel` (channel.rs:1317).
//!
//! This is deliberately distinct from the sibling `ChannelExtraQueryAdapter`
//! (in `wiring.rs`) which backs the channels *list page* extras
//! (`allChannelSummarys` / `allChannelTags` / `countChannelsByType` /
//! `queryChannels`) and from the `channel_ext` / `channel_ext2` mutation
//! traits — those are separate slices and are NOT re-implemented here.
//!
//! ## Go parity anchors (never guessed — read from `conduit/`)
//!   - `Query.channels` (`internal/server/gql/ent.resolvers.go:327`): the crate
//!     layer already lowered `orderBy` (`CREATED_AT` → ent `DefaultChannelOrder`
//!     = by id) into [`ChannelOrderSelection`]; the repo returns rows in
//!     `created_at ASC, id ASC` order, so we re-sort per the selection and apply
//!     Relay offset pagination in-memory (the channels table is small — same
//!     bounded-materialization strategy `ChannelExtraQueryAdapter` uses).
//!   - `createChannel` → `biz.ChannelService.CreateChannel`
//!     (`biz/channel.go:572`): duplicate-name check, create with ent column
//!     defaults (`status = disabled`, `autoSyncSupportedModels = false`,
//!     `orderingWeight = 0`); `input.credentials` IS persisted (the repo writes
//!     the `credentials` JSON column) even though the base GraphQL `Channel`
//!     type never exposes it back (`credentials` is `Sensitive()` in Go — see
//!     `conv::channel_row_to_gql`).
//!   - `updateChannel` → `biz.ChannelService.UpdateChannel`
//!     (`biz/channel.go:652`): duplicate-name check excluding self, then the
//!     partial merge whose exact field-application list is pinned by the
//!     in-memory service double in `channel.rs` (see `apply_update` mapping
//!     below): applied = type / baseURL(set only) / name / defaultTestModel /
//!     orderingWeight / autoSyncSupportedModels / supportedModels / manualModels
//!     / tags / settings / policies / credentials / remark(+clearRemark) /
//!     endpoints / clearAutoSyncModelPattern(wins over set) / clearErrorMessage.
//!     NOT applied by the Go service: status, error_message(set), clearBaseURL,
//!     and every `append*` variant.
//!   - `deleteChannel` → `biz.ChannelService.DeleteChannel`
//!     (`biz/channel.go:823`): the `Channel` ent schema carries the
//!     `SoftDeleteMixin`, so delete is a soft delete (sets `deleted_at`,
//!     `status = archived`), hiding the row from every default query — the
//!     observable admin-UI effect. The repo only exposes `soft_delete_channel`,
//!     matching that semantics (same as `ModelCrudAdapter::delete_model`).
//!
//! ## `where` predicate coverage (Query.channels)
//! ent lowers the full `ChannelWhereInput` predicate grammar; this adapter
//! materializes the live rows and filters in-memory. Covered: `not`/`and`/`or`
//! recursion plus every *scalar-column* predicate family — `type`, `status`,
//! `name`, `baseURL`, `defaultTestModel`, `autoSyncSupportedModels`,
//! `autoSyncModelPattern`, `orderingWeight`, `errorMessage`, `remark`
//! (comparison + nil families). Deferred (documented, rarely used on the
//! channels page): the `id` and `createdAt`/`updatedAt` predicate families and
//! the six `has<Edge>` existence predicates (they cross into other domains).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map as JsonMap, Value};

use conduit_admin_graphql::channel::{
    CapabilityPolicy, Channel as GqlChannel, ChannelConnection, ChannelConnectionArgs,
    ChannelCredentialsInput, ChannelEdge, ChannelEndpointInput, ChannelMutationServices,
    ChannelOrderTerm, ChannelPoliciesInput, ChannelQueryServices, ChannelServiceError,
    ChannelSettingsInput, ChannelStatus, ChannelType, ChannelWhereInput, CreateChannelInput,
    OAuthCredentialsInput, OrderDirection, OverrideOperationInput, ProxyConfigInput, ProxyType,
    UpdateChannelInput,
};
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::scalars::CursorScalar;
use conduit_core::objects::channel_settings as core_ch;
use conduit_core::objects::overrides as core_ov;
use conduit_db::repo::channel_repo::{
    CreateChannelInput as RepoCreateChannelInput, ListChannelsQuery,
    UpdateChannelInput as RepoUpdateChannelInput,
};
use conduit_db::row::ChannelRow;
use conduit_db::{ChannelRepo, PolicyContext, Principal, RepoError, RequestContext};

use crate::conv::channel_row_to_gql;

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing Channel base-domain adapter backed by the live
/// [`ChannelRepo`]. Implements both [`ChannelQueryServices`] and
/// [`ChannelMutationServices`].
pub struct ChannelCrudAdapter {
    channel_repo: Arc<dyn ChannelRepo>,
}

impl ChannelCrudAdapter {
    pub fn new(channel_repo: Arc<dyn ChannelRepo>) -> Self {
        Self { channel_repo }
    }

    /// Materialize every live (non-deleted) channel row, paging through the repo
    /// in generous windows. The channels table is small (tens of channels), so
    /// a full in-memory load mirrors Go's ent `.All(ctx)` faithfully.
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
}

/// The per-request context the host uses for repo calls. Mirrors
/// `wiring::boot_request_context` (a trusted, fully-authorized principal —
/// the admin GraphQL layer performs its own auth before reaching the service).
fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

/// Decode a GraphQL `ID!` (`gid://conduit/Channel/<n>` wire form or a bare
/// numeric id) into the numeric DB-id string the repo expects. Mirrors Go
/// `GUID.UnmarshalGQL`; a value that is neither is treated as "no such row".
fn channel_db_id(raw: &str) -> Option<String> {
    if let Ok(guid) = conduit_admin_graphql::node::parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// ChannelQueryServices — Query.channels
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelQueryServices for ChannelCrudAdapter {
    async fn channels(
        &self,
        args: ChannelConnectionArgs,
    ) -> Result<ChannelConnection, ChannelServiceError> {
        let rows = self.load_all().await?;

        // `where` filter (in-memory; see module doc for covered families).
        let mut rows: Vec<ChannelRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| channel_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Ordering: the crate already lowered `CREATED_AT` → `Id` (ent
        // DefaultChannelOrder). The repo returns created_at-asc order, so
        // re-sort for any explicit selection.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    ChannelOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    ChannelOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                    ChannelOrderTerm::Type => a.channel_type.cmp(&b.channel_type),
                    ChannelOrderTerm::Name => a.name.cmp(&b.name),
                    ChannelOrderTerm::Status => a.status.cmp(&b.status),
                    ChannelOrderTerm::OrderingWeight => a.ordering_weight.cmp(&b.ordering_weight),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let channels: Vec<GqlChannel> = rows.into_iter().map(channel_row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme (matching
        // `connection_from_offset_page`). A malformed `after` degrades to
        // offset 0 rather than failing the whole query.
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset)
            .unwrap_or(0)
            .min(channels.len());
        let windowed = channels[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

        Ok(ChannelConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(ChannelEdge {
                            node: Some(edge.node),
                            cursor: CursorScalar(edge.cursor),
                        })
                    })
                    .collect(),
            ),
            page_info: connection.page_info,
            total_count,
        })
    }
}

// ---------------------------------------------------------------------------
// ChannelMutationServices — create / update / delete
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelMutationServices for ChannelCrudAdapter {
    async fn create_channel(
        &self,
        input: CreateChannelInput,
    ) -> Result<GqlChannel, ChannelServiceError> {
        let ctx = boot_request_context();
        let now = chrono::Utc::now().to_rfc3339();
        // Retain the name for the duplicate-name error (the repo's
        // `NameConflict` maps to Go `xerrors.DuplicateNameError("channel", …)`).
        let name = input.name.clone();
        validate_official_openai_credentials(
            channel_type_to_wire(input.channel_type),
            input.base_url.as_deref(),
            &input.credentials,
        )
        .map_err(ChannelServiceError::Create)?;
        let website_url = normalize_website_url(input.website_url.clone())
            .map_err(ChannelServiceError::Create)?;
        let quota_currency = normalize_quota_currency(input.quota_currency.clone(), true)
            .map_err(ChannelServiceError::Create)?;
        let actual_quota_used = normalize_quota_amount(input.actual_quota_used.clone())
            .map_err(ChannelServiceError::Create)?;
        let quota_remaining = normalize_quota_amount(input.quota_remaining.clone())
            .map_err(ChannelServiceError::Create)?;

        let repo_input = RepoCreateChannelInput {
            // PostgreSQL owns the generated PK; `id` is ignored on
            // insert (read-back uses the DB id).
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
            created_at: now,
        };

        let row = self
            .channel_repo
            .create_channel(&ctx, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => ChannelServiceError::DuplicateName(name),
                other => ChannelServiceError::Create(other.to_string()),
            })?;
        Ok(channel_row_to_gql(row))
    }

    async fn update_channel(
        &self,
        id: &str,
        input: UpdateChannelInput,
    ) -> Result<GqlChannel, ChannelServiceError> {
        let ctx = boot_request_context();
        let db_id = channel_db_id(id).ok_or_else(|| {
            ChannelServiceError::Update(ChannelServiceError::NotFound.to_string())
        })?;
        let now = chrono::Utc::now().to_rfc3339();
        // Name (if any) retained for the duplicate-name error.
        let name = input.name.clone();
        let website_url = if input.clear_website_url.unwrap_or(false) {
            Some(None)
        } else if input.website_url.is_some() {
            Some(
                normalize_website_url(input.website_url.clone())
                    .map_err(ChannelServiceError::Update)?,
            )
        } else {
            None
        };
        let quota_currency = if input.quota_currency.is_some() {
            Some(
                normalize_quota_currency(input.quota_currency.clone(), false)
                    .map_err(ChannelServiceError::Update)?,
            )
        } else {
            None
        };
        let actual_quota_used = if input.clear_actual_quota_used.unwrap_or(false) {
            Some(None)
        } else if input.actual_quota_used.is_some() {
            Some(
                normalize_quota_amount(input.actual_quota_used.clone())
                    .map_err(ChannelServiceError::Update)?,
            )
        } else {
            None
        };
        let quota_remaining = if input.clear_quota_remaining.unwrap_or(false) {
            Some(None)
        } else if input.quota_remaining.is_some() {
            Some(
                normalize_quota_amount(input.quota_remaining.clone())
                    .map_err(ChannelServiceError::Update)?,
            )
        } else {
            None
        };
        if let Some(credentials) = input.credentials.as_ref() {
            let existing = self
                .channel_repo
                .find_channel(&ctx, &db_id)
                .await
                .map_err(|err| ChannelServiceError::Update(err.to_string()))?
                .ok_or_else(|| {
                    ChannelServiceError::Update(ChannelServiceError::NotFound.to_string())
                })?;
            let channel_type = input
                .channel_type
                .map(channel_type_to_wire)
                .unwrap_or(existing.channel_type.as_str());
            let base_url = input.base_url.as_deref().or(existing.base_url.as_deref());
            validate_official_openai_credentials(channel_type, base_url, credentials)
                .map_err(ChannelServiceError::Update)?;
        }

        // auto_sync_model_pattern: Go else-if — clear wins over a simultaneous
        // set. The repo stores an empty string as the cleared value.
        let auto_sync_model_pattern = if input.clear_auto_sync_model_pattern.unwrap_or(false) {
            Some(None)
        } else {
            input.auto_sync_model_pattern.map(Some)
        };
        // remark: Go SetRemark then ClearRemark — clear wins.
        let remark = if input.clear_remark.unwrap_or(false) {
            Some(None)
        } else {
            input.remark.map(Some)
        };
        // error_message: Go applies ClearErrorMessage only (never a Set).
        let error_message = if input.clear_error_message.unwrap_or(false) {
            Some(None)
        } else {
            None
        };

        let repo_input = RepoUpdateChannelInput {
            channel_type: input
                .channel_type
                .map(|t| channel_type_to_wire(t).to_string()),
            name: input.name,
            // Set-only (Go does not apply `clearBaseURL`).
            base_url: input.base_url.map(Some),
            website_url,
            quota_currency,
            actual_quota_used,
            quota_remaining,
            credentials: input.credentials.map(credentials_input_to_json),
            // Not present on `UpdateChannelInput`; managed elsewhere.
            disabled_api_keys: None,
            supported_models: input.supported_models,
            manual_models: input.manual_models,
            auto_sync_supported_models: input.auto_sync_supported_models,
            auto_sync_model_pattern,
            tags: input.tags,
            default_test_model: input.default_test_model,
            policies: input.policies.map(policies_input_to_json),
            settings: input.settings.map(settings_input_to_json),
            endpoints: input
                .endpoints
                .map(|v| v.into_iter().map(endpoint_input_to_json).collect()),
            remark,
            ordering_weight: input.ordering_weight,
            error_message,
            // Go's `UpdateChannel` does not touch `status`.
            status: None,
            updated_at: now,
        };

        let row = self
            .channel_repo
            .update_channel(&ctx, &db_id, repo_input)
            .await
            .map_err(|e| match e {
                RepoError::NameConflict => {
                    ChannelServiceError::DuplicateName(name.unwrap_or_default())
                }
                RepoError::NotFound(_) => {
                    ChannelServiceError::Update(ChannelServiceError::NotFound.to_string())
                }
                other => ChannelServiceError::Update(other.to_string()),
            })?;
        Ok(channel_row_to_gql(row))
    }

    async fn delete_channel(&self, id: &str) -> Result<(), ChannelServiceError> {
        let ctx = boot_request_context();
        let db_id = channel_db_id(id).ok_or_else(|| {
            ChannelServiceError::Delete(ChannelServiceError::NotFound.to_string())
        })?;
        let now = chrono::Utc::now().to_rfc3339();
        // Soft delete (ent SoftDeleteMixin) — hides the row from default queries.
        self.channel_repo
            .soft_delete_channel(&ctx, &db_id, &now)
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => {
                    ChannelServiceError::Delete(ChannelServiceError::NotFound.to_string())
                }
                other => ChannelServiceError::Delete(other.to_string()),
            })?;
        Ok(())
    }
}

pub(crate) fn normalize_website_url(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| "channel website URL must be a valid http:// or https:// URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("channel website URL must use http:// or https://".to_string());
    }
    Ok(Some(value.to_string()))
}

pub(crate) fn normalize_quota_currency(
    value: Option<String>,
    default_when_missing: bool,
) -> Result<Option<String>, String> {
    let value = match value {
        Some(value) => value.trim().to_ascii_uppercase(),
        None if default_when_missing => "USD".to_string(),
        None => return Ok(None),
    };
    if !(3..=8).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("quota currency must be a 3-8 letter currency code".to_string());
    }
    Ok(Some(value))
}

pub(crate) fn normalize_quota_amount(
    value: Option<conduit_admin_graphql::scalars::DecimalInputScalar>,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.0.is_sign_negative() {
        return Err("channel quota snapshot amounts cannot be negative".to_string());
    }
    Ok(Some(value.0.normalize().to_string()))
}

fn validate_official_openai_credentials(
    channel_type: &str,
    base_url: Option<&str>,
    credentials: &ChannelCredentialsInput,
) -> Result<(), String> {
    let is_official = channel_type == "openai"
        && base_url
            .map(str::trim)
            .map(|url| url.trim_end_matches('/'))
            .is_some_and(|url| url.eq_ignore_ascii_case("https://api.openai.com/v1"));
    if !is_official {
        return Ok(());
    }
    let invalid = credentials
        .api_keys
        .iter()
        .flatten()
        .chain(credentials.api_key.iter())
        .any(|key| !key.starts_with("sk-") || key.len() < 20);
    if invalid {
        Err("official OpenAI channels require a complete API key beginning with sk-; placeholder or short keys cannot be saved".into())
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Enum → wire-literal maps (reverse of `conv::channel_*_from_str`)
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

// ---------------------------------------------------------------------------
// GraphQL input → JSON column value (via the `conduit-core` typed objects,
// whose serde tags already mirror the Go `objects.*` JSON exactly; this is the
// exact shape `conv::channel_row_to_gql` reads back).
// ---------------------------------------------------------------------------

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
/// single-word tags `type`/`url`/`username`/`password`; `conv::proxy_value_to_gql`
/// reads them back defensively). Absent optionals are omitted.
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

/// Lower a `ChannelCredentialsInput` into the `credentials` JSON column value.
/// Built through the `conduit-core` `ChannelCredentials` (Go `objects.
/// ChannelCredentials` parity: `apiKey`/`apiKeys`/`gcp`/`oauth`). Credentials
/// are sensitive and never surfaced back on the GraphQL `Channel` type.
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
/// `oauth.OAuthCredentials` uses snake_case tags: `client_id`/`access_token`/
/// `refresh_token`/`expires_at`/`token_type`/`scopes`). Empty/absent optionals
/// are omitted where Go marks them `omitempty`.
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

// ---------------------------------------------------------------------------
// `where` predicate evaluation (Query.channels)
// ---------------------------------------------------------------------------

/// Whether a `ChannelRow` satisfies a `ChannelWhereInput` predicate tree.
/// `not`/`and`/`or` recurse; an empty `and` matches (ent semantics) and an
/// empty `or` is ignored so it never blacks out the whole result.
pub(crate) fn channel_row_matches_where(row: &ChannelRow, w: &ChannelWhereInput) -> bool {
    if let Some(inner) = &w.not
        && channel_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| channel_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| channel_row_matches_where(row, c))
    {
        return false;
    }

    if let Some(expected) = w.management_adapter.as_deref() {
        let actual = row
            .settings
            .get("managementAdapter")
            .or_else(|| row.settings.get("management_adapter"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            return false;
        }
    }

    // type enum predicates
    if let Some(t) = w.channel_type
        && row.channel_type != channel_type_to_wire(t)
    {
        return false;
    }
    if let Some(t) = w.type_neq
        && row.channel_type == channel_type_to_wire(t)
    {
        return false;
    }
    if let Some(list) = &w.type_in
        && !list
            .iter()
            .any(|t| row.channel_type == channel_type_to_wire(*t))
    {
        return false;
    }
    if let Some(list) = &w.type_not_in
        && list
            .iter()
            .any(|t| row.channel_type == channel_type_to_wire(*t))
    {
        return false;
    }

    // status enum predicates
    if let Some(s) = w.status
        && row.status != channel_status_to_wire(s)
    {
        return false;
    }
    if let Some(s) = w.status_neq
        && row.status == channel_status_to_wire(s)
    {
        return false;
    }
    if let Some(list) = &w.status_in
        && !list
            .iter()
            .any(|s| row.status == channel_status_to_wire(*s))
    {
        return false;
    }
    if let Some(list) = &w.status_not_in
        && list
            .iter()
            .any(|s| row.status == channel_status_to_wire(*s))
    {
        return false;
    }

    // name string family
    if !str_family(
        &row.name,
        &w.name,
        &w.name_neq,
        &w.name_in,
        &w.name_not_in,
        &w.name_gt,
        &w.name_gte,
        &w.name_lt,
        &w.name_lte,
        &w.name_contains,
        &w.name_has_prefix,
        &w.name_has_suffix,
        &w.name_equal_fold,
        &w.name_contains_fold,
    ) {
        return false;
    }

    // base_url string family + nil (nullable column)
    if w.base_url_is_nil == Some(true) && row.base_url.is_some() {
        return false;
    }
    if w.base_url_not_nil == Some(true) && row.base_url.is_none() {
        return false;
    }
    if !str_family(
        row.base_url.as_deref().unwrap_or(""),
        &w.base_url,
        &w.base_url_neq,
        &w.base_url_in,
        &w.base_url_not_in,
        &w.base_url_gt,
        &w.base_url_gte,
        &w.base_url_lt,
        &w.base_url_lte,
        &w.base_url_contains,
        &w.base_url_has_prefix,
        &w.base_url_has_suffix,
        &w.base_url_equal_fold,
        &w.base_url_contains_fold,
    ) {
        return false;
    }

    // default_test_model string family
    if !str_family(
        &row.default_test_model,
        &w.default_test_model,
        &w.default_test_model_neq,
        &w.default_test_model_in,
        &w.default_test_model_not_in,
        &w.default_test_model_gt,
        &w.default_test_model_gte,
        &w.default_test_model_lt,
        &w.default_test_model_lte,
        &w.default_test_model_contains,
        &w.default_test_model_has_prefix,
        &w.default_test_model_has_suffix,
        &w.default_test_model_equal_fold,
        &w.default_test_model_contains_fold,
    ) {
        return false;
    }

    // auto_sync_model_pattern string family + nil (empty string == nil, since
    // the column is COALESCE'd to '' on read)
    if w.auto_sync_model_pattern_is_nil == Some(true) && !row.auto_sync_model_pattern.is_empty() {
        return false;
    }
    if w.auto_sync_model_pattern_not_nil == Some(true) && row.auto_sync_model_pattern.is_empty() {
        return false;
    }
    if !str_family(
        &row.auto_sync_model_pattern,
        &w.auto_sync_model_pattern,
        &w.auto_sync_model_pattern_neq,
        &w.auto_sync_model_pattern_in,
        &w.auto_sync_model_pattern_not_in,
        &w.auto_sync_model_pattern_gt,
        &w.auto_sync_model_pattern_gte,
        &w.auto_sync_model_pattern_lt,
        &w.auto_sync_model_pattern_lte,
        &w.auto_sync_model_pattern_contains,
        &w.auto_sync_model_pattern_has_prefix,
        &w.auto_sync_model_pattern_has_suffix,
        &w.auto_sync_model_pattern_equal_fold,
        &w.auto_sync_model_pattern_contains_fold,
    ) {
        return false;
    }

    // error_message string family + nil (nullable column)
    if w.error_message_is_nil == Some(true) && row.error_message.is_some() {
        return false;
    }
    if w.error_message_not_nil == Some(true) && row.error_message.is_none() {
        return false;
    }
    if !str_family(
        row.error_message.as_deref().unwrap_or(""),
        &w.error_message,
        &w.error_message_neq,
        &w.error_message_in,
        &w.error_message_not_in,
        &w.error_message_gt,
        &w.error_message_gte,
        &w.error_message_lt,
        &w.error_message_lte,
        &w.error_message_contains,
        &w.error_message_has_prefix,
        &w.error_message_has_suffix,
        &w.error_message_equal_fold,
        &w.error_message_contains_fold,
    ) {
        return false;
    }

    // remark string family + nil (nullable column)
    if w.remark_is_nil == Some(true) && row.remark.is_some() {
        return false;
    }
    if w.remark_not_nil == Some(true) && row.remark.is_none() {
        return false;
    }
    if !str_family(
        row.remark.as_deref().unwrap_or(""),
        &w.remark,
        &w.remark_neq,
        &w.remark_in,
        &w.remark_not_in,
        &w.remark_gt,
        &w.remark_gte,
        &w.remark_lt,
        &w.remark_lte,
        &w.remark_contains,
        &w.remark_has_prefix,
        &w.remark_has_suffix,
        &w.remark_equal_fold,
        &w.remark_contains_fold,
    ) {
        return false;
    }

    // auto_sync_supported_models bool predicates
    if let Some(v) = w.auto_sync_supported_models
        && row.auto_sync_supported_models != v
    {
        return false;
    }
    if let Some(v) = w.auto_sync_supported_models_neq
        && row.auto_sync_supported_models == v
    {
        return false;
    }

    // ordering_weight numeric family
    if !i64_family(
        row.ordering_weight,
        &w.ordering_weight,
        &w.ordering_weight_neq,
        &w.ordering_weight_in,
        &w.ordering_weight_not_in,
        &w.ordering_weight_gt,
        &w.ordering_weight_gte,
        &w.ordering_weight_lt,
        &w.ordering_weight_lte,
    ) {
        return false;
    }

    true
}

/// Evaluate the full channel string-predicate family (eq/neq/in/notIn/
/// gt/gte/lt/lte/contains/hasPrefix/hasSuffix/equalFold/containsFold) against a
/// column value. `None` predicates are skipped (AND semantics, matching ent).
#[allow(clippy::too_many_arguments)]
fn str_family(
    value: &str,
    eq: &Option<String>,
    neq: &Option<String>,
    in_set: &Option<Vec<String>>,
    not_in: &Option<Vec<String>>,
    gt: &Option<String>,
    gte: &Option<String>,
    lt: &Option<String>,
    lte: &Option<String>,
    contains: &Option<String>,
    has_prefix: &Option<String>,
    has_suffix: &Option<String>,
    equal_fold: &Option<String>,
    contains_fold: &Option<String>,
) -> bool {
    if let Some(v) = eq
        && value != v
    {
        return false;
    }
    if let Some(v) = neq
        && value == v
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|x| x == value)
    {
        return false;
    }
    if let Some(v) = gt
        && (value <= v.as_str())
    {
        return false;
    }
    if let Some(v) = gte
        && (value < v.as_str())
    {
        return false;
    }
    if let Some(v) = lt
        && (value >= v.as_str())
    {
        return false;
    }
    if let Some(v) = lte
        && (value > v.as_str())
    {
        return false;
    }
    if let Some(v) = contains
        && !value.contains(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_prefix
        && !value.starts_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = has_suffix
        && !value.ends_with(v.as_str())
    {
        return false;
    }
    if let Some(v) = equal_fold
        && !value.eq_ignore_ascii_case(v)
    {
        return false;
    }
    if let Some(v) = contains_fold
        && !value.to_lowercase().contains(&v.to_lowercase())
    {
        return false;
    }
    true
}

/// Evaluate the numeric-predicate family (eq/neq/in/notIn/gt/gte/lt/lte) for an
/// `i64` column. `None` predicates are skipped (AND semantics).
#[allow(clippy::too_many_arguments)]
fn i64_family(
    value: i64,
    eq: &Option<i64>,
    neq: &Option<i64>,
    in_set: &Option<Vec<i64>>,
    not_in: &Option<Vec<i64>>,
    gt: &Option<i64>,
    gte: &Option<i64>,
    lt: &Option<i64>,
    lte: &Option<i64>,
) -> bool {
    if let Some(v) = eq
        && value != *v
    {
        return false;
    }
    if let Some(v) = neq
        && value == *v
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.contains(&value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.contains(&value)
    {
        return false;
    }
    if let Some(v) = gt
        && value <= *v
    {
        return false;
    }
    if let Some(v) = gte
        && value < *v
    {
        return false;
    }
    if let Some(v) = lt
        && value >= *v
    {
        return false;
    }
    if let Some(v) = lte
        && value > *v
    {
        return false;
    }
    true
}

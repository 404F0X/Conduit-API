//! RUST-P12-001 S07 — Channel CRUD GraphQL slice.
//!
//! Bounded scope (this file): the `channels` connection query plus the
//! `createChannel` / `updateChannel` / `deleteChannel` mutations and every
//! GraphQL type/input they reference. All shapes are copied field-for-field
//! from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Channel implements Node` (snapshot line 1741) — scalar/self-domain
//!     fields only; see "pending" note below.
//!   - `type ChannelConnection` / `type ChannelEdge` (lines 1873-1899).
//!   - `input CreateChannelInput` (lines 2896-2924, ent-generated).
//!   - `input UpdateChannelInput` (lines 7329-7372, ent-generated).
//!   - `input ChannelWhereInput` (lines 2659-2860, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `has<Edge>` booleans.
//!   - `input ChannelOrder` / `enum ChannelOrderField` (lines 2272-2292).
//!   - channel.graphql support types (lines 60-246): `ChannelEndpoint(Input)`,
//!     `ChannelSettings(Input)`, `ChannelPolicies(Input)`, `ChannelRateLimit
//!     (Input)`, `ChannelCredentialsInput`, `GCPCredentialInput`,
//!     `OAuthCredentialsInput`, `ModelMapping(Input)`, `ProxyConfig(Input)`,
//!     `TransformOptions(Input)`, `RetryableErrorPattern(Input)`,
//!     `OverrideOperation(Input)`, `OverrideMatch(Input)`.
//!   - enums `ChannelType` (58 values, line 2595), `ChannelStatus` (2587),
//!     `CapabilityPolicy` (144), `ProxyType` (77), `OrderDirection` (4072).
//!
//! Go reference implementations:
//!   - Query.channels           — `internal/server/gql/ent.resolvers.go:327`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultChannelOrder` = ID before
//!     delegating to ent `Paginate`).
//!   - Mutation.createChannel   — `internal/server/gql/conduit.resolvers.go:156`
//!     → `biz.ChannelService.CreateChannel` (`biz/channel.go:572`): duplicate
//!     name check → create with ent defaults → async channel reload.
//!   - Mutation.updateChannel   — `conduit.resolvers.go:171` →
//!     `biz.ChannelService.UpdateChannel` (`biz/channel.go:652`): duplicate
//!     name check (excluding self) → partial merge (see the in-memory test
//!     double for the exact field-application list) → async reload.
//!   - Mutation.deleteChannel   — `conduit.resolvers.go:186` →
//!     `biz.ChannelService.DeleteChannel` (`biz/channel.go:823`); resolver
//!     returns `false, err` on failure and `true` on success.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! These are cross-domain surfaces that belong to other task slices. They are
//! deliberately absent here (not silently dropped — tracked for follow-up):
//!
//!   - `Channel.requests(...): RequestConnection!` and
//!     `Channel.usageLogs(...): UsageLogConnection!` are **ported** in the
//!     RUST-P12-001 S07 continuation slice (see the `#[ComplexObject] impl
//!     Channel` block below); they reuse
//!     [`crate::request_usage::RequestQueryServices`] /
//!     [`crate::request_usage::UsageLogQueryServices`] and inject a
//!     `channelID = obj.id` predicate matching ent's edge machinery.
//!   - `Channel.executions(...): RequestExecutionConnection!`,
//!     `Channel.channelProbes: [ChannelProbe!]`,
//!     `Channel.channelModelPrices: [ChannelModelPrice!]`,
//!     `Channel.providerQuotaStatus: ProviderQuotaStatus` — edge fields into
//!     the request-execution / probe / price / provider-quota domains (their
//!     target types are not yet ported).
//!   - `extend type Channel` block (snapshot line 627): `defaultEndpoints`,
//!     `allModelEntries`, `credentials: ChannelCredentials`,
//!     `disabledAPIKeys`, `liveLimiterStats` (all Go force-resolvers).
//!   - `ChannelWhereInput.hasRequestsWith / hasExecutionsWith /
//!     hasUsageLogsWith / hasChannelProbesWith / hasChannelModelPricesWith /
//!     hasProviderQuotaStatusWith` — they reference other entities'
//!     `*WhereInput` types that are not ported yet.
//!   - Channel operations outside this batch: `duplicateChannel`,
//!     `bulkCreateChannels`, `saveChannelEndpoints`, `updateChannelStatus`,
//!     `bulk{Archive,Disable,Enable,Recover,Delete}Channels`, `testChannel*`,
//!     `bulkImportChannels`, `bulkUpdateChannelOrdering`, channel API-key
//!     management, channel override templates, `syncChannelModels`, and the
//!     extended queries `queryChannels` / `allChannelSummarys` /
//!     `allChannelTags` / `countChannelsByType`.
//!   - There is NO `channel(id: ID!)` single-object query in the snapshot;
//!     single-object lookup goes through the global `node(id: ID!)` /
//!     `nodes(ids: [ID!]!)` Relay queries (also a separate slice). The `Node`
//!     interface itself IS declared here so `type Channel implements Node`
//!     matches the snapshot; more implementors join as their slices land.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, Interface, Json, SimpleObject};

use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, DecimalInputScalar, DecimalScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; every value name is pinned explicitly
// because several are lowercase / snake_case, which the default
// SCREAMING_SNAKE renaming would mangle)
// ---------------------------------------------------------------------------

/// `enum ChannelStatus { enabled disabled archived }` — snapshot line 2587,
/// bound to Go `ent/channel.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum ChannelStatus {
    #[graphql(name = "enabled")]
    Enabled,
    #[graphql(name = "disabled")]
    Disabled,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum ChannelType` — snapshot lines 2595-2654 (58 values), bound to Go
/// `ent/channel.Type`. Values are snake_case in the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum ChannelType {
    #[graphql(name = "openai")]
    Openai,
    #[graphql(name = "openai_responses")]
    OpenaiResponses,
    #[graphql(name = "atlascloud")]
    Atlascloud,
    #[graphql(name = "codex")]
    Codex,
    #[graphql(name = "vercel")]
    Vercel,
    #[graphql(name = "anthropic")]
    Anthropic,
    #[graphql(name = "anthropic_aws")]
    AnthropicAws,
    #[graphql(name = "anthropic_gcp")]
    AnthropicGcp,
    #[graphql(name = "gemini_openai")]
    GeminiOpenai,
    #[graphql(name = "gemini")]
    Gemini,
    #[graphql(name = "gemini_vertex")]
    GeminiVertex,
    #[graphql(name = "deepseek")]
    Deepseek,
    #[graphql(name = "deepseek_anthropic")]
    DeepseekAnthropic,
    #[graphql(name = "deepinfra")]
    Deepinfra,
    #[graphql(name = "qiniu")]
    Qiniu,
    #[graphql(name = "fireworks")]
    Fireworks,
    #[graphql(name = "doubao")]
    Doubao,
    #[graphql(name = "doubao_anthropic")]
    DoubaoAnthropic,
    #[graphql(name = "moonshot")]
    Moonshot,
    #[graphql(name = "moonshot_anthropic")]
    MoonshotAnthropic,
    #[graphql(name = "zhipu")]
    Zhipu,
    #[graphql(name = "zai")]
    Zai,
    #[graphql(name = "zhipu_anthropic")]
    ZhipuAnthropic,
    #[graphql(name = "zai_anthropic")]
    ZaiAnthropic,
    #[graphql(name = "anthropic_fake")]
    AnthropicFake,
    #[graphql(name = "openai_fake")]
    OpenaiFake,
    #[graphql(name = "openrouter")]
    Openrouter,
    #[graphql(name = "xiaomi")]
    Xiaomi,
    #[graphql(name = "xiaomi_anthropic")]
    XiaomiAnthropic,
    #[graphql(name = "xai")]
    Xai,
    #[graphql(name = "ppio")]
    Ppio,
    #[graphql(name = "siliconflow")]
    Siliconflow,
    #[graphql(name = "volcengine")]
    Volcengine,
    #[graphql(name = "volcengine_anthropic")]
    VolcengineAnthropic,
    #[graphql(name = "longcat")]
    Longcat,
    #[graphql(name = "longcat_anthropic")]
    LongcatAnthropic,
    #[graphql(name = "minimax")]
    Minimax,
    #[graphql(name = "minimax_anthropic")]
    MinimaxAnthropic,
    #[graphql(name = "aihubmix")]
    Aihubmix,
    #[graphql(name = "aihubmix_anthropic")]
    AihubmixAnthropic,
    #[graphql(name = "burncloud")]
    Burncloud,
    #[graphql(name = "modelscope")]
    Modelscope,
    #[graphql(name = "bailian")]
    Bailian,
    #[graphql(name = "bailian_anthropic")]
    BailianAnthropic,
    #[graphql(name = "moonshot_coding")]
    MoonshotCoding,
    #[graphql(name = "jina")]
    Jina,
    #[graphql(name = "github")]
    Github,
    #[graphql(name = "github_copilot")]
    GithubCopilot,
    #[graphql(name = "claudecode")]
    Claudecode,
    #[graphql(name = "cerebras")]
    Cerebras,
    #[graphql(name = "antigravity")]
    Antigravity,
    #[graphql(name = "nanogpt")]
    Nanogpt,
    #[graphql(name = "nanogpt_responses")]
    NanogptResponses,
    #[graphql(name = "opencode_go")]
    OpencodeGo,
    #[graphql(name = "opencode_go_anthropic")]
    OpencodeGoAnthropic,
    #[graphql(name = "ollama")]
    Ollama,
    #[graphql(name = "evolink")]
    Evolink,
    #[graphql(name = "evolink_anthropic")]
    EvolinkAnthropic,
}

/// `enum CapabilityPolicy { unlimited require forbid }` — snapshot line 144.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum CapabilityPolicy {
    #[graphql(name = "unlimited")]
    Unlimited,
    #[graphql(name = "require")]
    Require,
    #[graphql(name = "forbid")]
    Forbid,
}

/// Stable, provider-neutral reason shown by the channel operations UI.  The
/// category is derived from already persisted channel/quota evidence; it does
/// not replace the original error text or the request execution record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelIssueCategory {
    #[graphql(name = "auth")]
    Auth,
    #[graphql(name = "quota")]
    Quota,
    #[graphql(name = "rate_limit")]
    RateLimit,
    #[graphql(name = "model")]
    Model,
    #[graphql(name = "unreachable")]
    Unreachable,
    #[graphql(name = "upstream")]
    Upstream,
    #[graphql(name = "protocol")]
    Protocol,
    #[graphql(name = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelIssueSeverity {
    #[graphql(name = "warning")]
    Warning,
    #[graphql(name = "error")]
    Error,
    #[graphql(name = "critical")]
    Critical,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelOperationalIssue {
    pub category: ChannelIssueCategory,
    pub severity: ChannelIssueSeverity,
    /// Stable machine-readable value suitable for filters and translations.
    pub code: String,
    /// Evidence lane: `channel_error` or `provider_quota`.
    pub source: String,
}

/// Proxy mode values consumed and produced by the retained frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ProxyType {
    #[graphql(name = "disabled")]
    Disabled,
    #[graphql(name = "environment")]
    Environment,
    #[graphql(name = "url")]
    Url,
}

/// `enum ChannelOrderField` — snapshot lines 2285-2292.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
    #[graphql(name = "TYPE")]
    Type,
    #[graphql(name = "NAME")]
    Name,
    #[graphql(name = "STATUS")]
    Status,
    #[graphql(name = "ORDERING_WEIGHT")]
    OrderingWeight,
}

/// `enum OrderDirection { ASC DESC }` — snapshot line 4072 (ent-global).
/// This is the GraphQL-facing enum; `crate::query::OrderDirection` is the
/// pure-parser twin used by the S06 helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum OrderDirection {
    #[graphql(name = "ASC")]
    Asc,
    #[graphql(name = "DESC")]
    Desc,
}

// ---------------------------------------------------------------------------
// Output object types (channel.graphql section of the snapshot)
// ---------------------------------------------------------------------------

/// `type ModelMapping { from: String! to: String! }` — snapshot line 11.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ModelMapping {
    pub from: String,
    pub to: String,
}

/// `type OverrideMatch { path: String! eq: String! }` — snapshot line 38.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct OverrideMatch {
    pub path: String,
    pub eq: String,
}

/// `type OverrideOperation` — snapshot lines 26-36.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct OverrideOperation {
    pub op: String,
    pub path: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub value: Option<String>,
    pub condition: Option<String>,
    // `match` is a Rust keyword; the GraphQL field name is pinned explicitly.
    #[graphql(name = "match")]
    pub match_: Option<OverrideMatch>,
    pub index: Option<i64>,
    pub splat: Option<bool>,
}

/// `type ProxyConfig` — snapshot lines 83-88.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ProxyConfig {
    // `type` is a Rust keyword; GraphQL name pinned explicitly.
    #[graphql(name = "type")]
    pub proxy_type: ProxyType,
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// `type TransformOptions` — snapshot lines 60-64 (all fields non-null).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct TransformOptions {
    pub force_array_instructions: bool,
    pub force_array_inputs: bool,
    pub replace_developer_role_with_system: bool,
}

/// `type RetryableErrorPattern { pattern: String! regex: Boolean! }` —
/// snapshot line 116.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct RetryableErrorPattern {
    pub pattern: String,
    pub regex: bool,
}

/// `type ChannelRateLimit` — snapshot lines 189-195 (all fields nullable Int).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelRateLimit {
    pub rpm: Option<i64>,
    pub tpm: Option<i64>,
    pub max_concurrent: Option<i64>,
    pub queue_size: Option<i64>,
    pub queue_timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "GCPCredential")]
pub struct GCPCredential {
    pub region: String,
    #[graphql(name = "projectID")]
    pub project_id: String,
    pub json_data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "OAuthCredentials")]
pub struct OAuthCredentials {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    #[graphql(name = "clientID")]
    pub client_id: Option<String>,
    pub expires_at: Option<TimeScalar>,
    pub token_type: Option<String>,
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelCredentials {
    pub api_key: Option<String>,
    pub api_keys: Option<Vec<String>>,
    pub gcp: Option<GCPCredential>,
    pub oauth: Option<OAuthCredentials>,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "DisabledAPIKey")]
pub struct DisabledAPIKey {
    pub key: String,
    pub disabled_at: TimeScalar,
    pub error_code: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelLimiterStats {
    pub in_flight: i64,
    pub waiting: i64,
    pub capacity: i64,
    pub queue_size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct AutoModelMappingRule {
    pub pattern: String,
    pub public_model_id_template: String,
    pub create_draft: bool,
    pub developer_template: Option<String>,
    pub name_template: Option<String>,
    pub group_template: Option<String>,
    pub model_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ErrorResponseRewriteRule {
    pub status_codes: Vec<i64>,
    pub body_pattern: Option<String>,
    pub http_status: Option<i64>,
    pub message: Option<String>,
    pub error_type: Option<String>,
    pub code: Option<String>,
    pub body: Option<Json<serde_json::Value>>,
}

/// `type ChannelPolicies { stream: CapabilityPolicy }` — snapshot line 150.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelPolicies {
    pub stream: Option<CapabilityPolicy>,
}

/// `type ChannelSettings` — snapshot lines 126-142.
///
/// `headerOverrideOperations` / `bodyOverrideOperations` are non-null lists in
/// the contract (Go resolves them via force-resolvers that fall back to an
/// empty slice for legacy rows — `conduit.resolvers.go`, the
/// `ParseOverrideOperations` backward-compat branch).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelSettings {
    pub management_adapter: Option<String>,
    pub billing_currency: Option<String>,
    pub recharge_multiplier: Option<DecimalScalar>,
    pub extra_model_prefix: Option<String>,
    pub model_mappings: Option<Vec<ModelMapping>>,
    // "Trimed" spelling is contract (snapshot line 129) — do not "fix" it.
    pub auto_trimed_model_prefixes: Option<Vec<String>>,
    pub hide_original_models: Option<bool>,
    pub hide_mapped_models: Option<bool>,
    pub lowercase_model_id: Option<bool>,
    pub proxy: Option<ProxyConfig>,
    pub transform_options: Option<TransformOptions>,
    pub header_override_operations: Vec<OverrideOperation>,
    pub body_override_operations: Vec<OverrideOperation>,
    pub pass_through_user_agent: Option<bool>,
    pub pass_through_body: Option<bool>,
    pub rate_limit: Option<ChannelRateLimit>,
    pub retryable_status_codes: Option<Vec<i64>>,
    pub retryable_error_patterns: Option<Vec<RetryableErrorPattern>>,
    pub auto_model_mapping_rules: Option<Vec<AutoModelMappingRule>>,
    pub error_response_rewrite_rules: Option<Vec<ErrorResponseRewriteRule>>,
}

/// `type ChannelEndpoint` — snapshot lines 90-95.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelEndpoint {
    pub api_format: String,
    pub path: Option<String>,
    // All-caps acronym tag: default camelCase would emit `baseUrl`.
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    pub transport: Option<String>,
}

fn default_channel_endpoints(channel_type: ChannelType) -> Vec<ChannelEndpoint> {
    const OPENAI_COMPATIBLE: &[&str] = &[
        "openai/chat_completions",
        "openai/embeddings",
        "openai/image_generation",
        "openai/image_edit",
        "openai/image_variation",
        "openai/video",
    ];
    const OPENAI_FULL: &[&str] = &[
        "openai/chat_completions",
        "openai/embeddings",
        "openai/image_generation",
        "openai/image_edit",
        "openai/image_variation",
        "openai/video",
        "openai/audio_speech",
        "openai/audio_transcriptions",
        "openai/audio_translations",
    ];
    const OPENAI_CHAT: &[&str] = &["openai/chat_completions"];
    const ANTHROPIC: &[&str] = &["anthropic/messages"];

    let formats: &[&str] = match channel_type {
        ChannelType::Openai | ChannelType::Nanogpt => OPENAI_FULL,
        ChannelType::Atlascloud
        | ChannelType::Vercel
        | ChannelType::Deepinfra
        | ChannelType::Ppio
        | ChannelType::Siliconflow
        | ChannelType::Aihubmix
        | ChannelType::Burncloud
        | ChannelType::Github
        | ChannelType::Evolink => OPENAI_COMPATIBLE,
        ChannelType::OpenaiResponses | ChannelType::NanogptResponses => &["openai/responses"],
        ChannelType::Codex => &[
            "openai/responses",
            "openai/image_generation",
            "openai/image_edit",
        ],
        ChannelType::Anthropic
        | ChannelType::AnthropicAws
        | ChannelType::AnthropicGcp
        | ChannelType::DeepseekAnthropic
        | ChannelType::DoubaoAnthropic
        | ChannelType::MoonshotAnthropic
        | ChannelType::ZhipuAnthropic
        | ChannelType::ZaiAnthropic
        | ChannelType::AnthropicFake
        | ChannelType::XiaomiAnthropic
        | ChannelType::VolcengineAnthropic
        | ChannelType::LongcatAnthropic
        | ChannelType::MinimaxAnthropic
        | ChannelType::AihubmixAnthropic
        | ChannelType::BailianAnthropic
        | ChannelType::MoonshotCoding
        | ChannelType::Claudecode
        | ChannelType::OpencodeGoAnthropic
        | ChannelType::EvolinkAnthropic => ANTHROPIC,
        ChannelType::GeminiOpenai
        | ChannelType::Qiniu
        | ChannelType::Fireworks
        | ChannelType::Moonshot
        | ChannelType::Zhipu
        | ChannelType::Zai
        | ChannelType::OpenaiFake
        | ChannelType::Xiaomi
        | ChannelType::Xai
        | ChannelType::Volcengine
        | ChannelType::Longcat
        | ChannelType::Minimax
        | ChannelType::Modelscope
        | ChannelType::Bailian
        | ChannelType::GithubCopilot
        | ChannelType::Cerebras
        | ChannelType::OpencodeGo => OPENAI_CHAT,
        ChannelType::Gemini | ChannelType::GeminiVertex => {
            &["gemini/contents", "gemini/embeddings"]
        }
        ChannelType::Deepseek => &["openai/chat_completions", "openai/completions"],
        ChannelType::Doubao => &["openai/chat_completions", "seedance/video"],
        ChannelType::Openrouter => &[
            "openai/chat_completions",
            "openai/audio_speech",
            "openai/audio_transcriptions",
            "openai/audio_translations",
        ],
        ChannelType::Jina => &["jina/rerank", "jina/embeddings"],
        ChannelType::Antigravity => &["gemini/contents"],
        ChannelType::Ollama => &["ollama/chat"],
    };

    formats
        .iter()
        .map(|api_format| ChannelEndpoint {
            api_format: (*api_format).to_owned(),
            path: Some(String::new()),
            base_url: Some(String::new()),
            transport: Some(String::new()),
        })
        .collect()
}

/// `type Channel implements Node` — snapshot lines 1741-1869, scalar and
/// channel-domain fields only. Cross-domain edge fields and the
/// `extend type Channel` force-resolver fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct Channel {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    #[graphql(name = "type")]
    pub channel_type: ChannelType,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    #[graphql(name = "websiteURL")]
    pub website_url: Option<String>,
    pub quota_currency: String,
    pub actual_quota_used: Option<DecimalScalar>,
    pub quota_remaining: Option<DecimalScalar>,
    pub name: String,
    pub status: ChannelStatus,
    pub supported_models: Vec<String>,
    pub manual_models: Option<Vec<String>>,
    pub auto_sync_supported_models: bool,
    pub auto_sync_model_pattern: Option<String>,
    pub tags: Option<Vec<String>>,
    pub default_test_model: String,
    pub policies: Option<ChannelPolicies>,
    pub settings: Option<ChannelSettings>,
    pub ordering_weight: i64,
    pub error_message: Option<String>,
    pub remark: Option<String>,
    pub endpoints: Option<Vec<ChannelEndpoint>>,
}

#[derive(Clone)]
struct DerivedModelEntry {
    actual_model: String,
    source: &'static str,
    priority: u8,
}

fn insert_model_entry(
    entries: &mut BTreeMap<String, DerivedModelEntry>,
    request_model: String,
    actual_model: String,
    source: &'static str,
    priority: u8,
) {
    let candidate = DerivedModelEntry {
        actual_model,
        source,
        priority,
    };
    entries
        .entry(request_model)
        .and_modify(|current| {
            if candidate.priority > current.priority {
                *current = candidate.clone();
            }
        })
        .or_insert(candidate);
}

fn channel_model_entries(channel: &Channel) -> Vec<crate::model_ext::ChannelModelEntry> {
    let settings = channel.settings.as_ref();
    let mut entries = BTreeMap::new();

    for model in &channel.supported_models {
        insert_model_entry(&mut entries, model.clone(), model.clone(), "direct", 4);
    }

    if let Some(prefix) = settings.and_then(|s| s.extra_model_prefix.as_deref())
        && !prefix.is_empty()
    {
        for model in &channel.supported_models {
            insert_model_entry(
                &mut entries,
                format!("{prefix}/{model}"),
                model.clone(),
                "prefix",
                1,
            );
        }
    }

    for prefix in settings
        .and_then(|s| s.auto_trimed_model_prefixes.as_deref())
        .unwrap_or_default()
    {
        if prefix.is_empty() {
            continue;
        }
        let needle = format!("{prefix}/");
        for model in &channel.supported_models {
            if let Some(trimmed) = model.strip_prefix(&needle) {
                insert_model_entry(
                    &mut entries,
                    trimmed.to_owned(),
                    model.clone(),
                    "auto_trim",
                    3,
                );
            }
        }
    }

    for mapping in settings
        .and_then(|s| s.model_mappings.as_deref())
        .unwrap_or_default()
    {
        if !channel.supported_models.contains(&mapping.to) {
            continue;
        }
        insert_model_entry(
            &mut entries,
            mapping.from.clone(),
            mapping.to.clone(),
            "mapping",
            2,
        );
        if settings.and_then(|s| s.hide_mapped_models).unwrap_or(false) {
            entries
                .retain(|_, entry| entry.source == "mapping" || entry.actual_model != mapping.to);
        }
    }

    if settings
        .and_then(|s| s.hide_original_models)
        .unwrap_or(false)
    {
        entries.retain(|_, entry| entry.source != "direct");
    }

    if settings.and_then(|s| s.lowercase_model_id).unwrap_or(false) {
        let mut lowercase_entries = BTreeMap::new();
        for (request_model, entry) in entries {
            insert_model_entry(
                &mut lowercase_entries,
                request_model.to_ascii_lowercase(),
                entry.actual_model,
                entry.source,
                entry.priority,
            );
        }
        entries = lowercase_entries;
    }

    entries
        .into_iter()
        .map(
            |(request_model, entry)| crate::model_ext::ChannelModelEntry {
                request_model,
                actual_model: entry.actual_model,
                source: entry.source.to_owned(),
            },
        )
        .collect()
}

// ---------------------------------------------------------------------------
// Channel edge fields — RUST-P12-001 S07 (continuation).
//
// The snapshot's `type Channel` block declares six cross-domain edge fields
// (snapshot lines 1773-1868):
//   - `requests(...): RequestConnection!`
//   - `executions(...): RequestExecutionConnection!`
//   - `usageLogs(...): UsageLogConnection!`
//   - `channelProbes: [ChannelProbe!]`
//   - `channelModelPrices: [ChannelModelPrice!]`
//   - `providerQuotaStatus: ProviderQuotaStatus`
//
// In Go these are auto-generated by ent as edge-connection resolvers: the
// generated code calls `obj.Queries().Request.Query()` (or similar) on the
// loaded `*ent.Channel`, paginates via ent's `Paginate`, and returns the
// `<Type>Connection`. Concretely the channel filter is injected by ent's
// edge machinery — the client passes the parent `*ent.Channel` and ent
// emits the `WHERE channel_id = ?` predicate.
//
// This slice ports the two highest-value edges whose target types already
// exist in this crate:
//   - `requests`  → reuses [`crate::request_usage::RequestQueryServices`].
//   - `usageLogs` → reuses [`crate::request_usage::UsageLogQueryServices`].
//
// Each resolver injects a `channelID: <obj.id>` predicate into the caller's
// `where` argument (matching ent's implicit edge filter) and delegates the
// rest of the args verbatim. The remaining four edges stay pending — their
// target types (`RequestExecution`, `ChannelProbe`, `ChannelModelPrice`,
// `ProviderQuotaStatus`) are not yet ported.
// ---------------------------------------------------------------------------

#[ComplexObject]
impl Channel {
    /// Built-in endpoint capabilities for the selected provider type. These
    /// are distinct from user overrides stored in `endpoints`.
    async fn default_endpoints(&self) -> Vec<ChannelEndpoint> {
        default_channel_endpoints(self.channel_type)
    }

    /// All request-model aliases accepted by this channel, including direct,
    /// prefixed, auto-trimmed, and explicitly mapped entries.
    async fn all_model_entries(&self) -> Vec<crate::model_ext::ChannelModelEntry> {
        channel_model_entries(self)
    }

    async fn channel_model_prices(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<Vec<crate::model_ext::ChannelModelPrice>>, String> {
        let prices = crate::channel_queries::channel_extra_query_services(ctx)?
            .channel_model_prices(self.id.as_str())
            .await
            .map_err(|err| err.to_string())?;
        Ok(Some(prices))
    }

    /// Sensitive channel fields are loaded only for callers holding the same
    /// `write_channels` capability required by the editor.
    async fn credentials(&self, ctx: &Context<'_>) -> Result<Option<ChannelCredentials>, String> {
        if crate::policy::authorize_current(ctx, "write_channels").is_err() {
            return Ok(None);
        }
        let services = crate::channel_queries::channel_extra_query_services(ctx)?;
        Ok(services
            .channel_sensitive_fields(self.id.as_str())
            .await
            .map_err(|err| err.to_string())?
            .and_then(|fields| fields.credentials))
    }

    #[graphql(name = "disabledAPIKeys")]
    async fn disabled_api_keys(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<Vec<DisabledAPIKey>>, String> {
        if crate::policy::authorize_current(ctx, "write_channels").is_err() {
            return Ok(None);
        }
        let services = crate::channel_queries::channel_extra_query_services(ctx)?;
        Ok(services
            .channel_sensitive_fields(self.id.as_str())
            .await
            .map_err(|err| err.to_string())?
            .map(|fields| fields.disabled_api_keys))
    }

    async fn live_limiter_stats(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<ChannelLimiterStats>, String> {
        let services = crate::channel_queries::channel_extra_query_services(ctx)?;
        services
            .live_limiter_stats(self.id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    /// `Channel.requests(...): RequestConnection!` — snapshot lines
    /// 1773-1808. Mirrors the ent-generated edge resolver: filter the
    /// request connection by `channelID = obj.id`, then delegate to the
    /// host-injected [`crate::request_usage::RequestQueryServices`].
    #[allow(clippy::too_many_arguments)]
    async fn requests(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<crate::request_usage::RequestOrder>,
        #[graphql(name = "where")] where_filter: Option<crate::request_usage::RequestWhereInput>,
    ) -> Result<crate::request_usage::RequestConnection, String> {
        let services = crate::request_usage::request_query_services(ctx)?;
        let injected = crate::request_usage::RequestWhereInput {
            channel_id: Some(self.id.clone()),
            ..where_filter.unwrap_or_default()
        };
        let args = crate::request_usage::RequestConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: crate::request_usage::resolve_request_order(order_by),
            where_filter: Some(injected),
        };
        services.requests(args).await.map_err(|err| err.to_string())
    }

    /// `Channel.usageLogs(...): UsageLogConnection!` — snapshot lines
    /// 1812-1860. Mirrors the ent-generated edge resolver: filter the
    /// usage-log connection by `channelID = obj.id`, then delegate to the
    /// host-injected [`crate::request_usage::UsageLogQueryServices`].
    #[allow(clippy::too_many_arguments)]
    async fn usage_logs(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<crate::request_usage::UsageLogOrder>,
        #[graphql(name = "where")] where_filter: Option<crate::request_usage::UsageLogWhereInput>,
    ) -> Result<crate::request_usage::UsageLogConnection, String> {
        let services = crate::request_usage::usage_log_query_services(ctx)?;
        let injected = crate::request_usage::UsageLogWhereInput {
            channel_id: Some(self.id.clone()),
            ..where_filter.unwrap_or_default()
        };
        let args = crate::request_usage::UsageLogConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: crate::request_usage::resolve_usage_log_order(order_by),
            where_filter: Some(injected),
        };
        services
            .usage_logs(args)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Channel.executions(...): RequestExecutionConnection!` — snapshot
    /// lines 464-494. Mirrors the ent-generated edge resolver
    /// (`generated.go:18477-18492` calls `obj.Executions(ctx, ...)` which
    /// issues `WHERE request_executions.channel_id = obj.ID` —
    /// `ent/channel/channel.go:86-91`: `ExecutionsTable =
    /// "request_executions"`, `ExecutionsColumn = "channel_id"`). We inject
    /// `channelID: <obj.id>` into the caller's `where` argument and delegate
    /// to the host-injected
    /// [`crate::request_execution::RequestExecutionQueryServices`].
    #[allow(clippy::too_many_arguments)]
    async fn executions(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<crate::request_execution::RequestExecutionOrder>,
        #[graphql(name = "where")] where_filter: Option<
            crate::request_execution::RequestExecutionWhereInput,
        >,
    ) -> Result<crate::request_execution::RequestExecutionConnection, String> {
        let services = crate::request_execution::request_execution_query_services(ctx)?;
        let injected = crate::request_execution::RequestExecutionWhereInput {
            channel_id: Some(self.id.clone()),
            ..where_filter.unwrap_or_default()
        };
        let args = crate::request_execution::RequestExecutionConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: crate::request_execution::resolve_request_execution_order(order_by),
            where_filter: Some(injected),
        };
        services
            .request_executions(args)
            .await
            .map_err(|err| err.to_string())
    }

    /// `Channel.providerQuotaStatus: ProviderQuotaStatus @goField(forceResolver:
    /// true)` — snapshot line 1868. Go `channelResolver.ProviderQuotaStatus`
    /// (`ent.resolvers.go:85-90`) calls the ent O2O edge `obj.ProviderQuotaStatus
    /// (ctx)` (FK `provider_quota_status.channel_id`) and returns `nil` when the
    /// channel has no quota-status row. Nullable in the contract.
    ///
    /// The resolver body lives in [`crate::provider_quota_ext`] so the type +
    /// enums + host trait seam stay in one self-contained module; this method is
    /// only the entry point because async-graphql forbids splitting a single
    /// `#[ComplexObject]` type across modules.
    async fn provider_quota_status(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<crate::provider_quota_ext::ProviderQuotaStatus>, String> {
        crate::provider_quota_ext::resolve_channel_provider_quota_status(ctx, &self.id).await
    }

    /// Structured summary for the operations UI. Traffic failures and account
    /// quota are intentionally separate evidence lanes; a management/account
    /// warning must not rewrite `Channel.status`.
    async fn operational_issue(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Option<ChannelOperationalIssue>, String> {
        let channel_issue = self
            .error_message
            .as_deref()
            .map(classify_channel_error_message);
        let quota_issue =
            crate::provider_quota_ext::resolve_channel_provider_quota_status(ctx, &self.id)
                .await?
                .and_then(|status| match status.status {
                    crate::provider_quota_ext::ProviderQuotaStatusStatus::Exhausted => {
                        Some(ChannelOperationalIssue {
                            category: ChannelIssueCategory::Quota,
                            severity: ChannelIssueSeverity::Critical,
                            code: "quota_exhausted".to_string(),
                            source: "provider_quota".to_string(),
                        })
                    }
                    crate::provider_quota_ext::ProviderQuotaStatusStatus::Warning => {
                        Some(ChannelOperationalIssue {
                            category: ChannelIssueCategory::Quota,
                            severity: ChannelIssueSeverity::Warning,
                            code: "quota_warning".to_string(),
                            source: "provider_quota".to_string(),
                        })
                    }
                    _ => None,
                });

        Ok(match (channel_issue, quota_issue) {
            (Some(channel), Some(quota)) => Some(
                if issue_priority(channel.category) >= issue_priority(quota.category) {
                    channel
                } else {
                    quota
                },
            ),
            (Some(issue), None) | (None, Some(issue)) => Some(issue),
            (None, None) => None,
        })
    }
}

fn issue_priority(category: ChannelIssueCategory) -> u8 {
    match category {
        ChannelIssueCategory::Auth => 8,
        ChannelIssueCategory::Quota => 7,
        ChannelIssueCategory::Protocol => 6,
        ChannelIssueCategory::Unreachable => 5,
        ChannelIssueCategory::Model => 4,
        ChannelIssueCategory::RateLimit => 3,
        ChannelIssueCategory::Upstream => 2,
        ChannelIssueCategory::Unknown => 1,
    }
}

fn classify_channel_error_message(message: &str) -> ChannelOperationalIssue {
    let normalized = message.to_ascii_lowercase();
    let contains_any = |needles: &[&str]| needles.iter().any(|needle| normalized.contains(needle));
    let (category, severity, code) = if contains_any(&[
        "unauthorized",
        "forbidden",
        "invalid api key",
        "invalid token",
        "authentication",
        "http 401",
        "http 403",
    ]) {
        (
            ChannelIssueCategory::Auth,
            ChannelIssueSeverity::Critical,
            "authentication_failed",
        )
    } else if contains_any(&[
        "insufficient quota",
        "quota exceeded",
        "quota exhausted",
        "insufficient balance",
        "credit balance",
        "额度不足",
        "余额不足",
    ]) {
        (
            ChannelIssueCategory::Quota,
            ChannelIssueSeverity::Critical,
            "quota_exhausted",
        )
    } else if contains_any(&["too many requests", "rate limit", "http 429"]) {
        (
            ChannelIssueCategory::RateLimit,
            ChannelIssueSeverity::Warning,
            "rate_limited",
        )
    } else if contains_any(&[
        "model not found",
        "unsupported model",
        "unknown model",
        "model mapping",
    ]) {
        (
            ChannelIssueCategory::Model,
            ChannelIssueSeverity::Error,
            "model_unavailable",
        )
    } else if contains_any(&[
        "connection refused",
        "connection reset",
        "dns",
        "tls",
        "certificate",
        "timed out",
        "timeout",
        "unreachable",
    ]) {
        (
            ChannelIssueCategory::Unreachable,
            ChannelIssueSeverity::Error,
            "endpoint_unreachable",
        )
    } else if contains_any(&[
        "malformed sse",
        "invalid envelope",
        "decode response",
        "decode error",
        "invalid json",
        "protocol",
    ]) {
        (
            ChannelIssueCategory::Protocol,
            ChannelIssueSeverity::Error,
            "protocol_incompatible",
        )
    } else if contains_any(&[
        "bad gateway",
        "service unavailable",
        "gateway timeout",
        "internal server error",
        "http 500",
        "http 502",
        "http 503",
        "http 504",
    ]) {
        (
            ChannelIssueCategory::Upstream,
            ChannelIssueSeverity::Error,
            "upstream_server_error",
        )
    } else {
        (
            ChannelIssueCategory::Unknown,
            ChannelIssueSeverity::Error,
            "unknown_channel_error",
        )
    };

    ChannelOperationalIssue {
        category,
        severity,
        code: code.to_string(),
        source: "channel_error".to_string(),
    }
}

/// Relay `interface Node { id: ID! }` — snapshot line 3848. Declared here so
/// `type Channel implements Node` renders exactly as the contract does.
/// Additional implementors (User, Project, …) join as their slices land; the
/// global `node`/`nodes` root queries are a separate slice.
/// `Model` joined with the S07 model-domain slice (`crate::model`).
// The enum exists to register the interface & its implementors in the SDL;
// instances are only built by the (future) node/nodes resolvers, so variant
// size is irrelevant and boxing would only complicate the Interface derive.
#[allow(clippy::large_enum_variant)]
#[derive(Interface)]
#[graphql(field(name = "id", ty = "&ID"))]
pub enum Node {
    Channel(Channel),
    Model(crate::model::Model),
    APIKey(crate::apikey::APIKey),
    APIKeyProfileTemplate(crate::profile_template::APIKeyProfileTemplate),
    Project(crate::project::Project),
    Role(crate::role::Role),
    User(crate::user::User),
    UserProject(crate::user::UserProject),
    UserRole(crate::user::UserRole),
    Prompt(crate::prompt::Prompt),
    PromptProtectionRule(crate::prompt::PromptProtectionRule),
    Thread(crate::threads_ext::Thread),
    Trace(crate::threads_ext::Trace),
    Request(crate::request_usage::Request),
    UsageLog(crate::request_usage::UsageLog),
}

/// `type ChannelEdge { node: Channel cursor: Cursor! }` — snapshot line 1890.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ChannelEdge {
    pub node: Option<Channel>,
    pub cursor: CursorScalar,
}

/// `type ChannelConnection` — snapshot line 1873. `edges` is a nullable list
/// of nullable edges (`[ChannelEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ChannelConnection {
    pub edges: Option<Vec<Option<ChannelEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input ModelMappingInput` — snapshot line 158.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ModelMappingInput {
    pub from: String,
    pub to: String,
}

/// `input OverrideMatchInput` — snapshot line 55.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct OverrideMatchInput {
    pub path: String,
    pub eq: String,
}

/// `input OverrideOperationInput` — snapshot lines 43-53.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct OverrideOperationInput {
    pub op: String,
    pub path: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub value: Option<String>,
    pub condition: Option<String>,
    #[graphql(name = "match")]
    pub match_: Option<OverrideMatchInput>,
    pub index: Option<i64>,
    pub splat: Option<bool>,
}

/// `input ProxyConfigInput` — snapshot lines 109-114.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ProxyConfigInput {
    #[graphql(name = "type")]
    pub proxy_type: ProxyType,
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// `input TransformOptionsInput` — snapshot lines 71-75 (all nullable, unlike
/// the output type whose fields are non-null).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct TransformOptionsInput {
    pub force_array_instructions: Option<bool>,
    pub force_array_inputs: Option<bool>,
    pub replace_developer_role_with_system: Option<bool>,
}

/// `input RetryableErrorPatternInput { pattern: String! regex: Boolean }` —
/// snapshot line 121 (`regex` nullable on input, zero-fills to false).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct RetryableErrorPatternInput {
    pub pattern: String,
    pub regex: Option<bool>,
}

/// `input ChannelRateLimitInput` — snapshot lines 181-187.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ChannelRateLimitInput {
    pub rpm: Option<i64>,
    pub tpm: Option<i64>,
    pub max_concurrent: Option<i64>,
    pub queue_size: Option<i64>,
    pub queue_timeout_ms: Option<i64>,
}

/// `input ChannelPoliciesInput { stream: CapabilityPolicy }` — snapshot 154.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ChannelPoliciesInput {
    pub stream: Option<CapabilityPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct AutoModelMappingRuleInput {
    pub pattern: String,
    pub public_model_id_template: String,
    pub create_draft: Option<bool>,
    pub developer_template: Option<String>,
    pub name_template: Option<String>,
    pub group_template: Option<String>,
    pub model_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ErrorResponseRewriteRuleInput {
    pub status_codes: Option<Vec<i64>>,
    pub body_pattern: Option<String>,
    pub http_status: Option<i64>,
    pub message: Option<String>,
    pub error_type: Option<String>,
    pub code: Option<String>,
    pub body: Option<Json<serde_json::Value>>,
}

/// `input ChannelSettingsInput` — snapshot lines 163-179 (all nullable; the
/// override-operation lists are nullable on input, non-null on output).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ChannelSettingsInput {
    pub management_adapter: Option<String>,
    pub billing_currency: Option<String>,
    pub recharge_multiplier: Option<DecimalInputScalar>,
    pub extra_model_prefix: Option<String>,
    pub model_mappings: Option<Vec<ModelMappingInput>>,
    pub auto_trimed_model_prefixes: Option<Vec<String>>,
    pub hide_original_models: Option<bool>,
    pub hide_mapped_models: Option<bool>,
    pub lowercase_model_id: Option<bool>,
    pub proxy: Option<ProxyConfigInput>,
    pub transform_options: Option<TransformOptionsInput>,
    pub header_override_operations: Option<Vec<OverrideOperationInput>>,
    pub body_override_operations: Option<Vec<OverrideOperationInput>>,
    pub pass_through_user_agent: Option<bool>,
    pub pass_through_body: Option<bool>,
    pub rate_limit: Option<ChannelRateLimitInput>,
    pub retryable_status_codes: Option<Vec<i64>>,
    pub retryable_error_patterns: Option<Vec<RetryableErrorPatternInput>>,
    pub auto_model_mapping_rules: Option<Vec<AutoModelMappingRuleInput>>,
    pub error_response_rewrite_rules: Option<Vec<ErrorResponseRewriteRuleInput>>,
}

/// Apply the shared channel recharge-metadata rules to a GraphQL settings
/// object before a mutation reaches its backing service.
pub fn validate_and_normalize_channel_settings_input(
    settings: Option<&mut ChannelSettingsInput>,
) -> Result<(), String> {
    let Some(settings) = settings else {
        return Ok(());
    };
    settings.billing_currency =
        conduit_core::objects::channel_settings::normalize_channel_billing_metadata(
            settings.billing_currency.as_deref(),
            settings
                .recharge_multiplier
                .as_ref()
                .map(|multiplier| &multiplier.0),
        )
        .map_err(|error| error.to_string())?;

    const MODEL_TYPES: &[&str] = &[
        "chat",
        "embedding",
        "rerank",
        "image_generation",
        "video_generation",
    ];
    for (index, rule) in settings
        .auto_model_mapping_rules
        .iter_mut()
        .flatten()
        .enumerate()
    {
        rule.pattern = rule.pattern.trim().to_string();
        rule.public_model_id_template = rule.public_model_id_template.trim().to_string();
        if rule.pattern.is_empty() || rule.public_model_id_template.is_empty() {
            return Err(format!(
                "auto model mapping rule {} requires pattern and publicModelIDTemplate",
                index + 1
            ));
        }
        regex::Regex::new(&rule.pattern).map_err(|error| {
            format!(
                "auto model mapping rule {} has invalid regex: {error}",
                index + 1
            )
        })?;
        let model_type = rule
            .model_type
            .as_deref()
            .unwrap_or("chat")
            .trim()
            .to_ascii_lowercase();
        if !MODEL_TYPES.contains(&model_type.as_str()) {
            return Err(format!(
                "auto model mapping rule {} has unsupported model type",
                index + 1
            ));
        }
        rule.model_type = Some(model_type);
    }
    for (index, rule) in settings
        .error_response_rewrite_rules
        .iter_mut()
        .flatten()
        .enumerate()
    {
        if rule
            .status_codes
            .iter()
            .flatten()
            .any(|status| !(400..=599).contains(status))
            || rule
                .http_status
                .is_some_and(|status| !(400..=599).contains(&status))
        {
            return Err(format!(
                "error response rewrite rule {} uses a status outside 400..599",
                index + 1
            ));
        }
        if let Some(pattern) = rule.body_pattern.as_mut() {
            *pattern = pattern.trim().to_string();
            if !pattern.is_empty() {
                regex::Regex::new(pattern).map_err(|error| {
                    format!(
                        "error response rewrite rule {} has invalid regex: {error}",
                        index + 1
                    )
                })?;
            }
        }
        if rule.http_status.is_none()
            && rule.message.is_none()
            && rule.error_type.is_none()
            && rule.code.is_none()
            && rule.body.is_none()
        {
            return Err(format!(
                "error response rewrite rule {} has no rewrite fields",
                index + 1
            ));
        }
    }
    Ok(())
}

/// `input ChannelEndpointInput` — snapshot lines 97-102.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelEndpointInput {
    pub api_format: String,
    pub path: Option<String>,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    pub transport: Option<String>,
}

/// `input GCPCredentialInput` — snapshot lines 242-246. The GraphQL type name
/// carries the all-caps `GCP` acronym, pinned explicitly.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "GCPCredentialInput")]
pub struct GcpCredentialInput {
    pub region: String,
    #[graphql(name = "projectID")]
    pub project_id: String,
    pub json_data: String,
}

/// `input OAuthCredentialsInput` — snapshot lines 233-240. The type name is
/// pinned explicitly: async-graphql's default ident normalization collapses
/// the `OA` acronym to `OauthCredentialsInput`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "OAuthCredentialsInput")]
pub struct OAuthCredentialsInput {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    #[graphql(name = "clientID")]
    pub client_id: Option<String>,
    pub expires_at: Option<TimeScalar>,
    pub token_type: Option<String>,
    pub scopes: Option<Vec<String>>,
}

/// `input ChannelCredentialsInput` — snapshot lines 226-231.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ChannelCredentialsInput {
    pub api_key: Option<String>,
    pub api_keys: Option<Vec<String>>,
    pub gcp: Option<GcpCredentialInput>,
    pub oauth: Option<OAuthCredentialsInput>,
}

/// `input CreateChannelInput` — snapshot lines 2896-2924 (ent-generated).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreateChannelInput {
    #[graphql(name = "type")]
    pub channel_type: ChannelType,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    #[graphql(name = "websiteURL")]
    pub website_url: Option<String>,
    pub quota_currency: Option<String>,
    pub actual_quota_used: Option<DecimalInputScalar>,
    pub quota_remaining: Option<DecimalInputScalar>,
    pub name: String,
    pub credentials: ChannelCredentialsInput,
    pub supported_models: Vec<String>,
    pub manual_models: Option<Vec<String>>,
    pub auto_sync_supported_models: Option<bool>,
    pub auto_sync_model_pattern: Option<String>,
    pub tags: Option<Vec<String>>,
    pub default_test_model: String,
    pub policies: Option<ChannelPoliciesInput>,
    pub settings: Option<ChannelSettingsInput>,
    pub ordering_weight: Option<i64>,
    pub remark: Option<String>,
    pub endpoints: Option<Vec<ChannelEndpointInput>>,
}

/// `input UpdateChannelInput` — snapshot lines 7329-7372 (ent-generated).
///
/// Note (Go parity): the ent input carries `append*`/`clear*` variants, but
/// `biz.ChannelService.UpdateChannel` only applies a subset — see the
/// in-memory double in the tests for the authoritative application list.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdateChannelInput {
    #[graphql(name = "type")]
    pub channel_type: Option<ChannelType>,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    #[graphql(name = "clearBaseURL")]
    pub clear_base_url: Option<bool>,
    #[graphql(name = "websiteURL")]
    pub website_url: Option<String>,
    pub clear_website_url: Option<bool>,
    pub quota_currency: Option<String>,
    pub actual_quota_used: Option<DecimalInputScalar>,
    pub clear_actual_quota_used: Option<bool>,
    pub quota_remaining: Option<DecimalInputScalar>,
    pub clear_quota_remaining: Option<bool>,
    pub name: Option<String>,
    pub status: Option<ChannelStatus>,
    pub credentials: Option<ChannelCredentialsInput>,
    pub supported_models: Option<Vec<String>>,
    pub append_supported_models: Option<Vec<String>>,
    pub manual_models: Option<Vec<String>>,
    pub append_manual_models: Option<Vec<String>>,
    pub clear_manual_models: Option<bool>,
    pub auto_sync_supported_models: Option<bool>,
    pub auto_sync_model_pattern: Option<String>,
    pub clear_auto_sync_model_pattern: Option<bool>,
    pub tags: Option<Vec<String>>,
    pub append_tags: Option<Vec<String>>,
    pub clear_tags: Option<bool>,
    pub default_test_model: Option<String>,
    pub policies: Option<ChannelPoliciesInput>,
    pub clear_policies: Option<bool>,
    pub settings: Option<ChannelSettingsInput>,
    pub clear_settings: Option<bool>,
    pub ordering_weight: Option<i64>,
    pub error_message: Option<String>,
    pub clear_error_message: Option<bool>,
    pub remark: Option<String>,
    pub clear_remark: Option<bool>,
    pub endpoints: Option<Vec<ChannelEndpointInput>>,
    pub append_endpoints: Option<Vec<ChannelEndpointInput>>,
    pub clear_endpoints: Option<bool>,
}

/// `input ChannelOrder { direction: OrderDirection! = ASC field:
/// ChannelOrderField! }` — snapshot lines 2272-2281.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: ChannelOrderField,
}

/// `input ChannelWhereInput` — snapshot lines 2659-2860 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family, and the `has<Edge>: Boolean` existence predicates. The
/// six `has<Edge>With: [<Other>WhereInput!]` fields are pending (they
/// reference other entities' WhereInputs — see module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ChannelWhereInput {
    pub not: Option<Box<ChannelWhereInput>>,
    pub and: Option<Vec<ChannelWhereInput>>,
    pub or: Option<Vec<ChannelWhereInput>>,
    // id field predicates
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    pub id_in: Option<Vec<ID>>,
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    pub created_at_in: Option<Vec<TimeScalar>>,
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    pub updated_at_in: Option<Vec<TimeScalar>>,
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // type field predicates
    #[graphql(name = "type")]
    pub channel_type: Option<ChannelType>,
    #[graphql(name = "typeNEQ")]
    pub type_neq: Option<ChannelType>,
    pub type_in: Option<Vec<ChannelType>>,
    pub type_not_in: Option<Vec<ChannelType>>,
    // base_url field predicates (all-caps acronym: every name pinned)
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    #[graphql(name = "baseURLNEQ")]
    pub base_url_neq: Option<String>,
    #[graphql(name = "baseURLIn")]
    pub base_url_in: Option<Vec<String>>,
    #[graphql(name = "baseURLNotIn")]
    pub base_url_not_in: Option<Vec<String>>,
    #[graphql(name = "baseURLGT")]
    pub base_url_gt: Option<String>,
    #[graphql(name = "baseURLGTE")]
    pub base_url_gte: Option<String>,
    #[graphql(name = "baseURLLT")]
    pub base_url_lt: Option<String>,
    #[graphql(name = "baseURLLTE")]
    pub base_url_lte: Option<String>,
    #[graphql(name = "baseURLContains")]
    pub base_url_contains: Option<String>,
    #[graphql(name = "baseURLHasPrefix")]
    pub base_url_has_prefix: Option<String>,
    #[graphql(name = "baseURLHasSuffix")]
    pub base_url_has_suffix: Option<String>,
    #[graphql(name = "baseURLIsNil")]
    pub base_url_is_nil: Option<bool>,
    #[graphql(name = "baseURLNotNil")]
    pub base_url_not_nil: Option<bool>,
    #[graphql(name = "baseURLEqualFold")]
    pub base_url_equal_fold: Option<String>,
    #[graphql(name = "baseURLContainsFold")]
    pub base_url_contains_fold: Option<String>,
    // name field predicates
    pub name: Option<String>,
    #[graphql(name = "nameNEQ")]
    pub name_neq: Option<String>,
    pub name_in: Option<Vec<String>>,
    pub name_not_in: Option<Vec<String>>,
    #[graphql(name = "nameGT")]
    pub name_gt: Option<String>,
    #[graphql(name = "nameGTE")]
    pub name_gte: Option<String>,
    #[graphql(name = "nameLT")]
    pub name_lt: Option<String>,
    #[graphql(name = "nameLTE")]
    pub name_lte: Option<String>,
    pub name_contains: Option<String>,
    pub name_has_prefix: Option<String>,
    pub name_has_suffix: Option<String>,
    pub name_equal_fold: Option<String>,
    pub name_contains_fold: Option<String>,
    // status field predicates
    pub status: Option<ChannelStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<ChannelStatus>,
    pub status_in: Option<Vec<ChannelStatus>>,
    pub status_not_in: Option<Vec<ChannelStatus>>,
    // auto_sync_supported_models field predicates
    pub auto_sync_supported_models: Option<bool>,
    #[graphql(name = "autoSyncSupportedModelsNEQ")]
    pub auto_sync_supported_models_neq: Option<bool>,
    // auto_sync_model_pattern field predicates
    pub auto_sync_model_pattern: Option<String>,
    #[graphql(name = "autoSyncModelPatternNEQ")]
    pub auto_sync_model_pattern_neq: Option<String>,
    pub auto_sync_model_pattern_in: Option<Vec<String>>,
    pub auto_sync_model_pattern_not_in: Option<Vec<String>>,
    #[graphql(name = "autoSyncModelPatternGT")]
    pub auto_sync_model_pattern_gt: Option<String>,
    #[graphql(name = "autoSyncModelPatternGTE")]
    pub auto_sync_model_pattern_gte: Option<String>,
    #[graphql(name = "autoSyncModelPatternLT")]
    pub auto_sync_model_pattern_lt: Option<String>,
    #[graphql(name = "autoSyncModelPatternLTE")]
    pub auto_sync_model_pattern_lte: Option<String>,
    pub auto_sync_model_pattern_contains: Option<String>,
    pub auto_sync_model_pattern_has_prefix: Option<String>,
    pub auto_sync_model_pattern_has_suffix: Option<String>,
    pub auto_sync_model_pattern_is_nil: Option<bool>,
    pub auto_sync_model_pattern_not_nil: Option<bool>,
    pub auto_sync_model_pattern_equal_fold: Option<String>,
    pub auto_sync_model_pattern_contains_fold: Option<String>,
    // default_test_model field predicates
    pub default_test_model: Option<String>,
    #[graphql(name = "defaultTestModelNEQ")]
    pub default_test_model_neq: Option<String>,
    pub default_test_model_in: Option<Vec<String>>,
    pub default_test_model_not_in: Option<Vec<String>>,
    #[graphql(name = "defaultTestModelGT")]
    pub default_test_model_gt: Option<String>,
    #[graphql(name = "defaultTestModelGTE")]
    pub default_test_model_gte: Option<String>,
    #[graphql(name = "defaultTestModelLT")]
    pub default_test_model_lt: Option<String>,
    #[graphql(name = "defaultTestModelLTE")]
    pub default_test_model_lte: Option<String>,
    pub default_test_model_contains: Option<String>,
    pub default_test_model_has_prefix: Option<String>,
    pub default_test_model_has_suffix: Option<String>,
    pub default_test_model_equal_fold: Option<String>,
    pub default_test_model_contains_fold: Option<String>,
    // ordering_weight field predicates
    pub ordering_weight: Option<i64>,
    #[graphql(name = "orderingWeightNEQ")]
    pub ordering_weight_neq: Option<i64>,
    pub ordering_weight_in: Option<Vec<i64>>,
    pub ordering_weight_not_in: Option<Vec<i64>>,
    #[graphql(name = "orderingWeightGT")]
    pub ordering_weight_gt: Option<i64>,
    #[graphql(name = "orderingWeightGTE")]
    pub ordering_weight_gte: Option<i64>,
    #[graphql(name = "orderingWeightLT")]
    pub ordering_weight_lt: Option<i64>,
    #[graphql(name = "orderingWeightLTE")]
    pub ordering_weight_lte: Option<i64>,
    // error_message field predicates
    pub error_message: Option<String>,
    #[graphql(name = "errorMessageNEQ")]
    pub error_message_neq: Option<String>,
    pub error_message_in: Option<Vec<String>>,
    pub error_message_not_in: Option<Vec<String>>,
    #[graphql(name = "errorMessageGT")]
    pub error_message_gt: Option<String>,
    #[graphql(name = "errorMessageGTE")]
    pub error_message_gte: Option<String>,
    #[graphql(name = "errorMessageLT")]
    pub error_message_lt: Option<String>,
    #[graphql(name = "errorMessageLTE")]
    pub error_message_lte: Option<String>,
    pub error_message_contains: Option<String>,
    pub error_message_has_prefix: Option<String>,
    pub error_message_has_suffix: Option<String>,
    pub error_message_is_nil: Option<bool>,
    pub error_message_not_nil: Option<bool>,
    pub error_message_equal_fold: Option<String>,
    pub error_message_contains_fold: Option<String>,
    // remark field predicates
    pub remark: Option<String>,
    #[graphql(name = "remarkNEQ")]
    pub remark_neq: Option<String>,
    pub remark_in: Option<Vec<String>>,
    pub remark_not_in: Option<Vec<String>>,
    #[graphql(name = "remarkGT")]
    pub remark_gt: Option<String>,
    #[graphql(name = "remarkGTE")]
    pub remark_gte: Option<String>,
    #[graphql(name = "remarkLT")]
    pub remark_lt: Option<String>,
    #[graphql(name = "remarkLTE")]
    pub remark_lte: Option<String>,
    pub remark_contains: Option<String>,
    pub remark_has_prefix: Option<String>,
    pub remark_has_suffix: Option<String>,
    pub remark_is_nil: Option<bool>,
    pub remark_not_nil: Option<bool>,
    pub remark_equal_fold: Option<String>,
    pub remark_contains_fold: Option<String>,
    // edge existence predicates (`has<Edge>With` variants pending — they
    // reference other entities' WhereInput types, see module doc)
    pub has_requests: Option<bool>,
    pub has_executions: Option<bool>,
    pub has_usage_logs: Option<bool>,
    pub has_channel_probes: Option<bool>,
    pub has_channel_model_prices: Option<bool>,
    pub has_provider_quota_status: Option<bool>,
    /// Conduit API extension: filter by the control-plane adapter stored in
    /// `Channel.settings.managementAdapter` (for example `new_api`).
    pub management_adapter: Option<String>,
}

// ---------------------------------------------------------------------------
// Input → object conversions.
//
// Go binds the input and output GraphQL types to the SAME `objects.*` Go
// struct via `@goModel` (e.g. `ChannelSettingsInput` and `ChannelSettings`
// both map to `objects.ChannelSettings`), so an input value round-trips into
// the stored object unchanged. These `From` impls are the Rust analogue;
// nullable input fields that back non-null output fields zero-fill exactly
// like gqlgen decoding into a plain Go bool/slice.
// ---------------------------------------------------------------------------

impl From<ModelMappingInput> for ModelMapping {
    fn from(input: ModelMappingInput) -> Self {
        Self {
            from: input.from,
            to: input.to,
        }
    }
}

impl From<OverrideMatchInput> for OverrideMatch {
    fn from(input: OverrideMatchInput) -> Self {
        Self {
            path: input.path,
            eq: input.eq,
        }
    }
}

impl From<OverrideOperationInput> for OverrideOperation {
    fn from(input: OverrideOperationInput) -> Self {
        Self {
            op: input.op,
            path: input.path,
            from: input.from,
            to: input.to,
            value: input.value,
            condition: input.condition,
            match_: input.match_.map(OverrideMatch::from),
            index: input.index,
            splat: input.splat,
        }
    }
}

impl From<ProxyConfigInput> for ProxyConfig {
    fn from(input: ProxyConfigInput) -> Self {
        Self {
            proxy_type: input.proxy_type,
            url: Some(input.url.unwrap_or_default()),
            username: Some(input.username.unwrap_or_default()),
            password: Some(input.password.unwrap_or_default()),
        }
    }
}

impl From<TransformOptionsInput> for TransformOptions {
    fn from(input: TransformOptionsInput) -> Self {
        // Output fields are non-null Booleans; omitted input fields zero-fill
        // to false (gqlgen decodes `Boolean` into a plain Go bool).
        Self {
            force_array_instructions: input.force_array_instructions.unwrap_or(false),
            force_array_inputs: input.force_array_inputs.unwrap_or(false),
            replace_developer_role_with_system: input
                .replace_developer_role_with_system
                .unwrap_or(false),
        }
    }
}

impl From<RetryableErrorPatternInput> for RetryableErrorPattern {
    fn from(input: RetryableErrorPatternInput) -> Self {
        Self {
            pattern: input.pattern,
            regex: input.regex.unwrap_or(false),
        }
    }
}

impl From<ChannelRateLimitInput> for ChannelRateLimit {
    fn from(input: ChannelRateLimitInput) -> Self {
        Self {
            rpm: input.rpm,
            tpm: input.tpm,
            max_concurrent: input.max_concurrent,
            queue_size: input.queue_size,
            queue_timeout_ms: input.queue_timeout_ms,
        }
    }
}

impl From<ChannelPoliciesInput> for ChannelPolicies {
    fn from(input: ChannelPoliciesInput) -> Self {
        Self {
            stream: input.stream,
        }
    }
}

impl From<ChannelEndpointInput> for ChannelEndpoint {
    fn from(input: ChannelEndpointInput) -> Self {
        Self {
            api_format: input.api_format,
            path: Some(input.path.unwrap_or_default()),
            base_url: Some(input.base_url.unwrap_or_default()),
            transport: Some(input.transport.unwrap_or_default()),
        }
    }
}

impl From<ChannelSettingsInput> for ChannelSettings {
    fn from(input: ChannelSettingsInput) -> Self {
        // NOTE: `ModelMapping::from` / `OverrideOperation::from` cannot be
        // used as function paths here — those structs carry a GraphQL field
        // named `from`, and SimpleObject generates an inherent resolver
        // method of the same name that shadows the `From` trait method.
        // `Into::into` resolves through the trait unambiguously.
        Self {
            management_adapter: input.management_adapter,
            billing_currency: input.billing_currency,
            recharge_multiplier: input
                .recharge_multiplier
                .map(|value| DecimalScalar(value.0)),
            extra_model_prefix: input.extra_model_prefix,
            model_mappings: input
                .model_mappings
                .map(|v| v.into_iter().map(Into::into).collect()),
            auto_trimed_model_prefixes: input.auto_trimed_model_prefixes,
            hide_original_models: input.hide_original_models,
            hide_mapped_models: input.hide_mapped_models,
            lowercase_model_id: input.lowercase_model_id,
            proxy: input.proxy.map(ProxyConfig::from),
            transform_options: input.transform_options.map(TransformOptions::from),
            // Non-null output lists; Go's force-resolver falls back to an
            // empty slice when unset (conduit.resolvers.go override-ops
            // backward-compat branch).
            header_override_operations: input
                .header_override_operations
                .map(|v| v.into_iter().map(Into::into).collect())
                .unwrap_or_default(),
            body_override_operations: input
                .body_override_operations
                .map(|v| v.into_iter().map(Into::into).collect())
                .unwrap_or_default(),
            pass_through_user_agent: input.pass_through_user_agent,
            pass_through_body: input.pass_through_body,
            rate_limit: input.rate_limit.map(ChannelRateLimit::from),
            retryable_status_codes: input.retryable_status_codes,
            retryable_error_patterns: input
                .retryable_error_patterns
                .map(|v| v.into_iter().map(RetryableErrorPattern::from).collect()),
            auto_model_mapping_rules: input.auto_model_mapping_rules.map(|rules| {
                rules
                    .into_iter()
                    .map(|rule| AutoModelMappingRule {
                        pattern: rule.pattern,
                        public_model_id_template: rule.public_model_id_template,
                        create_draft: rule.create_draft.unwrap_or(true),
                        developer_template: rule.developer_template,
                        name_template: rule.name_template,
                        group_template: rule.group_template,
                        model_type: rule.model_type.unwrap_or_else(|| "chat".to_string()),
                    })
                    .collect()
            }),
            error_response_rewrite_rules: input.error_response_rewrite_rules.map(|rules| {
                rules
                    .into_iter()
                    .map(|rule| ErrorResponseRewriteRule {
                        status_codes: rule.status_codes.unwrap_or_default(),
                        body_pattern: rule.body_pattern,
                        http_status: rule.http_status,
                        message: rule.message,
                        error_type: rule.error_type,
                        code: rule.code,
                        body: rule.body,
                    })
                    .collect()
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:327-336)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of
/// the GraphQL `ChannelOrderField` enum — it is ent's `DefaultChannelOrder`
/// (order by primary key), which the Go resolver substitutes when the client
/// asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOrderTerm {
    /// ent `DefaultChannelOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
    Type,
    Name,
    Status,
    OrderingWeight,
}

/// The resolver-lowered ordering selection handed to
/// [`ChannelQueryServices::channels`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelOrderSelection {
    pub direction: OrderDirection,
    pub term: ChannelOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.channels` (ent.resolvers.go:328-330): a `CREATED_AT`
/// request is remapped to `ent.DefaultChannelOrder` (order by ID) with the
/// requested direction preserved; every other field maps one-to-one.
pub fn resolve_channel_order(order_by: Option<ChannelOrder>) -> Option<ChannelOrderSelection> {
    order_by.map(|order| ChannelOrderSelection {
        direction: order.direction,
        term: match order.field {
            ChannelOrderField::CreatedAt => ChannelOrderTerm::Id,
            ChannelOrderField::UpdatedAt => ChannelOrderTerm::UpdatedAt,
            ChannelOrderField::Type => ChannelOrderTerm::Type,
            ChannelOrderField::Name => ChannelOrderTerm::Name,
            ChannelOrderField::Status => ChannelOrderTerm::Status,
            ChannelOrderField::OrderingWeight => ChannelOrderTerm::OrderingWeight,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependencies:
// `r.client.Channel` for the connection query and `r.channelService` for the
// CRUD mutations)
// ---------------------------------------------------------------------------

/// Error surface for the channel services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("channel", name)`
///     (`internal/pkg/xerrors/graphql.go:104`).
///   - not found — ent's `ent: channel not found`.
///   - wrapped create/update/delete failures — the `fmt.Errorf("failed to
///     ...: %w")` prefixes in `biz/channel.go`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ChannelServiceError {
    #[error("channel service is not available")]
    ServiceUnavailable,
    #[error("channel name '{0}' already exists")]
    DuplicateName(String),
    #[error("ent: channel not found")]
    NotFound,
    #[error("failed to create channel: {0}")]
    Create(String),
    #[error("failed to update channel: {0}")]
    Update(String),
    #[error("failed to delete channel: {0}")]
    Delete(String),
    #[error("failed to query channels: {0}")]
    Query(String),
}

/// Arguments for the `channels` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
/// Cursor values are the raw `Cursor` scalar strings; `where_filter` is the
/// full predicate input — lowering it into storage predicates is the host's
/// job (ent does this in Go via `WithChannelFilter`).
#[derive(Debug, Clone, Default)]
pub struct ChannelConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ChannelOrderSelection>,
    pub where_filter: Option<ChannelWhereInput>,
}

/// Backs `Query.channels` (Go ent.resolvers.go:327: `r.client.Channel.Query()
/// .Paginate(...)`). The host wires the real repository; tests use an
/// in-memory store.
#[async_trait::async_trait]
pub trait ChannelQueryServices: Send + Sync {
    async fn channels(
        &self,
        args: ChannelConnectionArgs,
    ) -> Result<ChannelConnection, ChannelServiceError>;
}

/// Backs the three CRUD mutations (Go `biz.ChannelService`). `id` is the raw
/// GraphQL `ID!` scalar string; Go decodes it into `objects.GUID` and passes
/// `.ID` (int) to the service — concrete hosts perform the same decode.
#[async_trait::async_trait]
pub trait ChannelMutationServices: Send + Sync {
    /// Mirrors `ChannelService.CreateChannel` (biz/channel.go:572): reject
    /// duplicate names, create with ent column defaults (status `disabled`,
    /// autoSyncSupportedModels `false`, orderingWeight `0`), then trigger the
    /// async channel reload.
    async fn create_channel(
        &self,
        input: CreateChannelInput,
    ) -> Result<Channel, ChannelServiceError>;

    /// Mirrors `ChannelService.UpdateChannel` (biz/channel.go:652): duplicate
    /// name check excluding self, then a partial merge of the provided
    /// fields.
    async fn update_channel(
        &self,
        id: &str,
        input: UpdateChannelInput,
    ) -> Result<Channel, ChannelServiceError>;

    /// Mirrors `ChannelService.DeleteChannel` (biz/channel.go:823): delete by
    /// id, forget the channel's limiter, trigger the async reload.
    async fn delete_channel(&self, id: &str) -> Result<(), ChannelServiceError>;
}

/// Resolves the injected [`ChannelQueryServices`] from the async-graphql data
/// bag; absent wiring surfaces the "service is not available" failure mode
/// (same convention as `mutation::quota_services`).
pub(crate) fn channel_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelQueryServices>, String> {
    match ctx.data::<Arc<dyn ChannelQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ChannelServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`ChannelMutationServices`] from the data bag.
pub(crate) fn channel_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelMutationServices>, String> {
    match ctx.data::<Arc<dyn ChannelMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ChannelServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    use async_graphql::ID;

    use super::*;
    use crate::pagination::connection_from_offset_page;
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go `biz.ChannelService` call
    // sequences without DB/HTTP; the connection query mirrors the thin ent
    // delegation (no predicate lowering — the `where` filter is recorded and
    // passed through, as in Go where ent lowers it).
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryChannelService {
        channels: Arc<Mutex<Vec<Channel>>>,
        captured_query_args: Arc<Mutex<Vec<ChannelConnectionArgs>>>,
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Fixed timestamp for stored rows (the wire format of `Time` is not
    /// under test here).
    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_channel(id: i64, name: &str) -> Channel {
        Channel {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            channel_type: ChannelType::Openai,
            base_url: None,
            website_url: None,
            quota_currency: "USD".to_string(),
            actual_quota_used: None,
            quota_remaining: None,
            name: name.to_owned(),
            status: ChannelStatus::Enabled,
            supported_models: vec!["gpt-4o".to_owned()],
            manual_models: None,
            auto_sync_supported_models: false,
            auto_sync_model_pattern: None,
            tags: None,
            default_test_model: "gpt-4o".to_owned(),
            policies: None,
            settings: None,
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: None,
        }
    }

    #[async_trait::async_trait]
    impl ChannelQueryServices for InMemoryChannelService {
        async fn channels(
            &self,
            args: ChannelConnectionArgs,
        ) -> Result<ChannelConnection, ChannelServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<Channel> = lock(&self.channels).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        ChannelOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        ChannelOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
                        ChannelOrderTerm::Type => a.channel_type.cmp(&b.channel_type),
                        ChannelOrderTerm::Name => a.name.cmp(&b.name),
                        ChannelOrderTerm::Status => a.status.cmp(&b.status),
                        ChannelOrderTerm::OrderingWeight => {
                            a.ordering_weight.cmp(&b.ordering_weight)
                        }
                    };
                    match selection.direction {
                        OrderDirection::Asc => ordering,
                        OrderDirection::Desc => ordering.reverse(),
                    }
                });
            }

            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
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

    #[async_trait::async_trait]
    impl ChannelMutationServices for InMemoryChannelService {
        async fn create_channel(
            &self,
            input: CreateChannelInput,
        ) -> Result<Channel, ChannelServiceError> {
            let mut guard = lock(&self.channels);
            // Go CreateChannel (biz/channel.go:572): duplicate-name check
            // first — `xerrors.DuplicateNameError("channel", input.Name)`.
            if guard.iter().any(|existing| existing.name == input.name) {
                return Err(ChannelServiceError::DuplicateName(input.name));
            }

            // ent column defaults (internal/ent/schema/channel.go): status
            // defaults to `disabled` (the create input skips status),
            // auto_sync_supported_models to false, ordering_weight to 0.
            let id = guard.len() as i64 + 1;
            let created = Channel {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                channel_type: input.channel_type,
                base_url: input.base_url,
                website_url: input.website_url,
                quota_currency: input.quota_currency.unwrap_or_else(|| "USD".to_string()),
                actual_quota_used: input.actual_quota_used.map(|value| DecimalScalar(value.0)),
                quota_remaining: input.quota_remaining.map(|value| DecimalScalar(value.0)),
                name: input.name,
                status: ChannelStatus::Disabled,
                supported_models: input.supported_models,
                manual_models: input.manual_models,
                auto_sync_supported_models: input.auto_sync_supported_models.unwrap_or(false),
                auto_sync_model_pattern: input.auto_sync_model_pattern,
                tags: input.tags,
                default_test_model: input.default_test_model,
                policies: input.policies.map(ChannelPolicies::from),
                settings: input.settings.map(ChannelSettings::from),
                ordering_weight: input.ordering_weight.unwrap_or(0),
                error_message: None,
                remark: input.remark,
                // input.credentials is persisted in Go but the base GraphQL
                // Channel type does not expose credentials (extend-block
                // force-resolver, pending slice) — nothing to store here.
                endpoints: input
                    .endpoints
                    .map(|v| v.into_iter().map(ChannelEndpoint::from).collect()),
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_channel(
            &self,
            id: &str,
            input: UpdateChannelInput,
        ) -> Result<Channel, ChannelServiceError> {
            let mut guard = lock(&self.channels);
            // Go UpdateChannel (biz/channel.go:652): the duplicate-name check
            // (excluding self) runs BEFORE the UpdateOneID save.
            if let Some(name) = &input.name
                && guard
                    .iter()
                    .any(|other| other.name == *name && other.id.as_str() != id)
            {
                return Err(ChannelServiceError::DuplicateName(name.clone()));
            }

            let Some(channel) = guard.iter_mut().find(|c| c.id.as_str() == id) else {
                // ent UpdateOneID on a missing row fails at Save; biz wraps it
                // as "failed to update channel: %w".
                return Err(ChannelServiceError::Update(
                    ChannelServiceError::NotFound.to_string(),
                ));
            };

            // Field application mirrors Go biz/channel.go:652-767 exactly.
            // Applied: SetNillable{Type,BaseURL,Name,DefaultTestModel,
            // OrderingWeight,AutoSyncSupportedModels}; conditional Set for
            // supportedModels/manualModels/tags/settings/policies/
            // credentials/remark/endpoints; ClearRemark;
            // ClearAutoSyncModelPattern (clear wins over set — Go else-if);
            // ClearErrorMessage.
            // NOT applied by the Go service (parity): status, errorMessage,
            // clearBaseURL, append*/clear* variants for supported/manual
            // models, tags, policies, settings, endpoints.
            if let Some(v) = input.channel_type {
                channel.channel_type = v;
            }
            if let Some(v) = input.base_url {
                channel.base_url = Some(v);
            }
            if let Some(v) = input.name {
                channel.name = v;
            }
            if let Some(v) = input.default_test_model {
                channel.default_test_model = v;
            }
            if let Some(v) = input.ordering_weight {
                channel.ordering_weight = v;
            }
            if let Some(v) = input.auto_sync_supported_models {
                channel.auto_sync_supported_models = v;
            }
            if let Some(v) = input.supported_models {
                channel.supported_models = v;
            }
            if let Some(v) = input.manual_models {
                channel.manual_models = Some(v);
            }
            if let Some(v) = input.tags {
                channel.tags = Some(v);
            }
            if let Some(v) = input.settings {
                channel.settings = Some(ChannelSettings::from(v));
            }
            if let Some(v) = input.policies {
                channel.policies = Some(ChannelPolicies::from(v));
            }
            // input.credentials: applied in Go (mut.SetCredentials) but not
            // representable on the base Channel type — see create_channel.
            if let Some(v) = input.remark {
                channel.remark = Some(v);
            }
            if input.clear_remark.unwrap_or(false) {
                channel.remark = None;
            }
            if input.clear_auto_sync_model_pattern.unwrap_or(false) {
                channel.auto_sync_model_pattern = None;
            } else if let Some(v) = input.auto_sync_model_pattern {
                channel.auto_sync_model_pattern = Some(v);
            }
            if let Some(v) = input.endpoints {
                channel.endpoints = Some(v.into_iter().map(ChannelEndpoint::from).collect());
            }
            if input.clear_error_message.unwrap_or(false) {
                channel.error_message = None;
            }

            Ok(channel.clone())
        }

        async fn delete_channel(&self, id: &str) -> Result<(), ChannelServiceError> {
            let mut guard = lock(&self.channels);
            let before = guard.len();
            guard.retain(|c| c.id.as_str() != id);
            if guard.len() == before {
                // Go: "failed to delete channel: %w" wrapping ent not-found.
                return Err(ChannelServiceError::Delete(
                    ChannelServiceError::NotFound.to_string(),
                ));
            }
            Ok(())
        }
    }

    fn schema_with(store: &InMemoryChannelService) -> AdminSchema {
        let query: Arc<dyn ChannelQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ChannelMutationServices> = Arc::new(store.clone());
        admin_schema_builder().data(query).data(mutation).finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // ---------------------------------------------------------------------
    // SDL block comparison helpers. The snapshot at
    // tests/contracts/admin_graphql_schema.graphql is the oracle: for each
    // channel-domain type we extract the field/value lines from BOTH the
    // generated SDL and the snapshot and require set equality (minus the
    // documented pending fields). This simultaneously catches missing fields,
    // wrong types/nullability, wrong casing, and invented fields.
    // ---------------------------------------------------------------------

    fn snapshot_text() -> Result<String, TestError> {
        std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| format!("snapshot read failed: {err}").into())
    }

    /// Strip a trailing ` @directive(...)` (snapshot uses `@goField` /
    /// `@goModel`) and a trailing default value (` = ASC`) so both SDL
    /// dialects normalize to plain `name: Type` lines. Also collapse inline
    /// argument lists `name(args): Type` (async-graphql's single-line form)
    /// down to `name(…): Type` so they compare equal against the snapshot's
    /// multi-line form which `block_fields` collapses the same way.
    fn normalize_field_line(line: &str) -> String {
        let without_directive = match line.find(" @") {
            Some(pos) => line[..pos].trim_end(),
            None => line,
        };
        let without_default = match without_directive.find(" = ") {
            Some(pos) => without_directive[..pos].trim_end(),
            None => without_directive,
        };
        // Collapse inline args: `name(arg: Type, ...): RetType` → `name(…): RetType`.
        if let (Some(open), Some(close)) = (without_default.find('('), without_default.find("): "))
            && open < close
        {
            let name = without_default[..open].trim_end();
            let return_type = &without_default[close + 3..];
            return format!("{name}(…): {return_type}");
        }
        without_default.to_owned()
    }

    /// Extract the normalized field/value lines of the `header` type block
    /// (e.g. `header = "input ChannelWhereInput"`). Handles the snapshot's
    /// `"""` docstrings, CRLF line endings, trailing directives, and
    /// gqlgen-style multi-line argument fields (collapsed to
    /// `name(…): ReturnType`).
    fn block_fields(text: &str, header: &str) -> Result<BTreeSet<String>, TestError> {
        let mut lines = text.lines().map(str::trim);
        let exact_open = format!("{header} {{");
        let prefixed_open = format!("{header} ");
        let mut found = false;
        for line in lines.by_ref() {
            if line == exact_open || (line.starts_with(&prefixed_open) && line.ends_with('{')) {
                found = true;
                break;
            }
        }
        if !found {
            return Err(format!("block `{header}` not found").into());
        }

        let mut fields = BTreeSet::new();
        let mut in_doc = false;
        while let Some(line) = lines.next() {
            if line.starts_with("\"\"\"") {
                // Single-line docstrings (`"""text"""`) do not toggle.
                let single_line = line.len() > 3 && line.ends_with("\"\"\"");
                if !single_line {
                    in_doc = !in_doc;
                }
                continue;
            }
            if in_doc {
                continue;
            }
            if line == "}" {
                return Ok(fields);
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(field_name) = line.strip_suffix('(') {
                // gqlgen multi-line argument field: consume until `): Type`.
                let mut inner_doc = false;
                for arg_line in lines.by_ref() {
                    if arg_line.starts_with("\"\"\"") {
                        let single_line = arg_line.len() > 3 && arg_line.ends_with("\"\"\"");
                        if !single_line {
                            inner_doc = !inner_doc;
                        }
                        continue;
                    }
                    if inner_doc {
                        continue;
                    }
                    if let Some(return_type) = arg_line.strip_prefix("): ") {
                        fields.insert(format!(
                            "{field_name}(…): {}",
                            normalize_field_line(return_type)
                        ));
                        break;
                    }
                }
                continue;
            }
            fields.insert(normalize_field_line(line));
        }
        Err(format!("block `{header}` never closed").into())
    }

    /// Assert the generated block equals the snapshot block minus the listed
    /// pending fields; also assert nothing was invented (generated ⊆ snapshot)
    /// and that every pending field really exists in the snapshot.
    fn assert_block_parity(
        sdl: &str,
        snapshot: &str,
        ours_header: &str,
        snapshot_header: &str,
        pending: &[&str],
    ) -> Result<(), TestError> {
        let ours = block_fields(sdl, ours_header)?;
        let mut expected = block_fields(snapshot, snapshot_header)?;
        for pending_field in pending {
            assert!(
                expected.remove(*pending_field),
                "pending field `{pending_field}` not found in snapshot block `{snapshot_header}` — stale pending list"
            );
        }
        let missing: Vec<_> = expected.difference(&ours).collect();
        let invented: Vec<_> = ours.difference(&expected).collect();
        assert!(
            missing.is_empty() && invented.is_empty(),
            "block `{snapshot_header}` drift — missing from Rust SDL: {missing:?}; invented (not in snapshot): {invented:?}"
        );
        Ok(())
    }

    fn assert_block_parity_with_extensions(
        sdl: &str,
        snapshot: &str,
        ours_header: &str,
        snapshot_header: &str,
        pending: &[&str],
        extensions: &[&str],
    ) -> Result<(), TestError> {
        let mut ours = block_fields(sdl, ours_header)?;
        for extension in extensions {
            assert!(
                ours.remove(*extension),
                "frontend extension `{extension}` missing from `{ours_header}`"
            );
        }
        let mut expected = block_fields(snapshot, snapshot_header)?;
        for pending_field in pending {
            assert!(expected.remove(*pending_field));
        }
        let missing: Vec<_> = expected.difference(&ours).collect();
        let invented: Vec<_> = ours.difference(&expected).collect();
        assert!(
            missing.is_empty() && invented.is_empty(),
            "block `{snapshot_header}` drift — missing: {missing:?}; unapproved extensions: {invented:?}"
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: object types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_channel_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Cross-domain edge fields whose target types are NOT yet ported
        // remain pending. `requests`/`usageLogs`/`executions` were ported in
        // the RUST-P12-001 S07 continuation slices (this module doc +
        // `request_execution.rs`); `providerQuotaStatus` was ported in GAP-C
        // (`crate::provider_quota_ext`) — they are therefore NOT in the
        // pending list anymore.
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type Channel",
            "type Channel",
            &["channelProbes: [ChannelProbe!]"],
            &[
                "allModelEntries: [ChannelModelEntry!]!",
                "credentials: ChannelCredentials",
                "defaultEndpoints: [ChannelEndpoint!]!",
                "disabledAPIKeys: [DisabledAPIKey!]",
                "liveLimiterStats: ChannelLimiterStats",
                "actualQuotaUsed: Decimal",
                "quotaCurrency: String!",
                "quotaRemaining: Decimal",
                "websiteURL: String",
                "operationalIssue: ChannelOperationalIssue",
            ],
        )?;

        // The implements clause must match the snapshot's declaration.
        assert!(
            sdl.contains("type Channel implements Node {"),
            "generated SDL must declare `type Channel implements Node`"
        );
        assert!(snapshot.contains("type Channel implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_channel_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ChannelConnection",
            "type ChannelConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type ChannelEdge", "type ChannelEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_channel_support_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type ChannelEndpoint",
            "type ChannelPolicies",
            "type ChannelRateLimit",
            "type ModelMapping",
            "type ProxyConfig",
            "type TransformOptions",
            "type RetryableErrorPattern",
            "type OverrideOperation",
            "type OverrideMatch",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type ChannelSettings",
            "type ChannelSettings",
            &[],
            &[
                "managementAdapter: String",
                // Rust-owned billing metadata used to normalize provider
                // balance units into the configured accounting currency.
                "billingCurrency: String",
                "rechargeMultiplier: Decimal",
                "autoModelMappingRules: [AutoModelMappingRule!]",
                "errorResponseRewriteRules: [ErrorResponseRewriteRule!]",
            ],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_create_channel_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input CreateChannelInput",
            "input CreateChannelInput",
            &[],
            &[
                "actualQuotaUsed: DecimalInput",
                "quotaCurrency: String",
                "quotaRemaining: DecimalInput",
                "websiteURL: String",
            ],
        )
    }

    #[test]
    fn sdl_update_channel_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input UpdateChannelInput",
            "input UpdateChannelInput",
            &[],
            &[
                "actualQuotaUsed: DecimalInput",
                "clearActualQuotaUsed: Boolean",
                "clearQuotaRemaining: Boolean",
                "clearWebsiteUrl: Boolean",
                "quotaCurrency: String",
                "quotaRemaining: DecimalInput",
                "websiteURL: String",
            ],
        )
    }

    #[test]
    fn sdl_channel_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError>
    {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        // The six `has<Edge>With` fields reference other entities' WhereInput
        // types (pending slices).
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input ChannelWhereInput",
            "input ChannelWhereInput",
            &[
                "hasRequestsWith: [RequestWhereInput!]",
                "hasExecutionsWith: [RequestExecutionWhereInput!]",
                "hasUsageLogsWith: [UsageLogWhereInput!]",
                "hasChannelProbesWith: [ChannelProbeWhereInput!]",
                "hasChannelModelPricesWith: [ChannelModelPriceWhereInput!]",
                "hasProviderQuotaStatusWith: [ProviderQuotaStatusWhereInput!]",
            ],
            &["managementAdapter: String"],
        )
    }

    #[test]
    fn sdl_channel_support_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input ChannelEndpointInput",
            "input ChannelPoliciesInput",
            "input ChannelRateLimitInput",
            "input ChannelCredentialsInput",
            "input GCPCredentialInput",
            "input OAuthCredentialsInput",
            "input ModelMappingInput",
            "input ProxyConfigInput",
            "input TransformOptionsInput",
            "input RetryableErrorPatternInput",
            "input OverrideOperationInput",
            "input OverrideMatchInput",
            "input ChannelOrder",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input ChannelSettingsInput",
            "input ChannelSettingsInput",
            &[],
            &[
                "managementAdapter: String",
                "billingCurrency: String",
                "rechargeMultiplier: DecimalInput",
                "autoModelMappingRules: [AutoModelMappingRuleInput!]",
                "errorResponseRewriteRules: [ErrorResponseRewriteRuleInput!]",
            ],
        )?;
        // The block comparison strips default values; pin the `= ASC`
        // default of ChannelOrder.direction exactly (snapshot line 2276).
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "generated SDL must render the ASC default on ChannelOrder.direction"
        );
        assert!(snapshot.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: enums
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_channel_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "enum ChannelType",
            "enum ChannelStatus",
            "enum CapabilityPolicy",
            "enum ChannelOrderField",
            "enum OrderDirection",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        let proxy_values = block_fields(&sdl, "enum ProxyType")?;
        assert_eq!(
            proxy_values,
            ["disabled", "environment", "url"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            "ProxyType is a frontend-facing lower-case enum"
        );
        // Spot-check the value count of the big enum (58 channel types).
        let values = block_fields(&sdl, "enum ChannelType")?;
        assert_eq!(values.len(), 58, "ChannelType must carry 58 values");
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: root operation signatures
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_channels_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query.channels — async-graphql renders arguments inline; the
        // snapshot spreads them across lines (type Query, lines 5279-5309).
        assert!(
            sdl.contains(
                "channels(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: ChannelOrder, where: ChannelWhereInput): ChannelConnection!"
            ),
            "generated SDL missing the channels connection signature: {sdl}"
        );
        for token in [
            "after: Cursor",
            "first: Int",
            "before: Cursor",
            "last: Int",
            "orderBy: ChannelOrder",
            "where: ChannelWhereInput",
            "): ChannelConnection!",
        ] {
            assert!(
                snapshot.contains(token),
                "snapshot missing channels arg token `{token}`"
            );
        }

        // Mutations — flat one-line signatures in both dialects
        // (snapshot type Mutation, lines 789/792/795).
        for signature in [
            "createChannel(input: CreateChannelInput!): Channel!",
            "updateChannel(id: ID!, input: UpdateChannelInput!): Channel!",
            "deleteChannel(id: ID!): Boolean!",
        ] {
            assert!(
                sdl.contains(signature),
                "generated SDL missing `{signature}`"
            );
            assert!(
                snapshot.contains(signature),
                "snapshot missing `{signature}`"
            );
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Ordering lowering (Go ent.resolvers.go:328-330)
    // ---------------------------------------------------------------------

    #[test]
    fn resolve_channel_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_channel_order(Some(ChannelOrder {
            direction: OrderDirection::Desc,
            field: ChannelOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(ChannelOrderSelection {
                direction: OrderDirection::Desc,
                term: ChannelOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_channel_order_maps_other_fields_one_to_one() {
        for (field, term) in [
            (ChannelOrderField::UpdatedAt, ChannelOrderTerm::UpdatedAt),
            (ChannelOrderField::Type, ChannelOrderTerm::Type),
            (ChannelOrderField::Name, ChannelOrderTerm::Name),
            (ChannelOrderField::Status, ChannelOrderTerm::Status),
            (
                ChannelOrderField::OrderingWeight,
                ChannelOrderTerm::OrderingWeight,
            ),
        ] {
            let selection = resolve_channel_order(Some(ChannelOrder {
                direction: OrderDirection::Asc,
                field,
            }));
            assert_eq!(
                selection.map(|s| s.term),
                Some(term),
                "field {field:?} must map to {term:?}"
            );
        }
        assert_eq!(resolve_channel_order(None), None);
    }

    // ---------------------------------------------------------------------
    // Input → object conversion semantics
    // ---------------------------------------------------------------------

    #[test]
    fn settings_conversion_zero_fills_non_null_lists_and_bools() {
        let settings = ChannelSettings::from(ChannelSettingsInput {
            management_adapter: Some("new_api".to_string()),
            transform_options: Some(TransformOptionsInput::default()),
            ..ChannelSettingsInput::default()
        });
        // Non-null contract lists fall back to empty (Go force-resolver).
        assert!(settings.header_override_operations.is_empty());
        assert!(settings.body_override_operations.is_empty());
        assert_eq!(settings.management_adapter.as_deref(), Some("new_api"));
        // Nullable input bools zero-fill into the non-null output bools.
        assert_eq!(
            settings.transform_options,
            Some(TransformOptions {
                force_array_instructions: false,
                force_array_inputs: false,
                replace_developer_role_with_system: false,
            })
        );
    }

    #[test]
    fn channel_error_classifier_returns_stable_categories() {
        for (message, category, code) in [
            (
                "HTTP 401 Unauthorized",
                ChannelIssueCategory::Auth,
                "authentication_failed",
            ),
            (
                "insufficient quota",
                ChannelIssueCategory::Quota,
                "quota_exhausted",
            ),
            (
                "429 Too Many Requests",
                ChannelIssueCategory::RateLimit,
                "rate_limited",
            ),
            (
                "unsupported model",
                ChannelIssueCategory::Model,
                "model_unavailable",
            ),
            (
                "connection refused",
                ChannelIssueCategory::Unreachable,
                "endpoint_unreachable",
            ),
            (
                "malformed SSE frame",
                ChannelIssueCategory::Protocol,
                "protocol_incompatible",
            ),
            (
                "502 Bad Gateway",
                ChannelIssueCategory::Upstream,
                "upstream_server_error",
            ),
        ] {
            let issue = classify_channel_error_message(message);
            assert_eq!(issue.category, category, "message: {message}");
            assert_eq!(issue.code, code, "message: {message}");
        }
    }

    #[test]
    fn override_operation_conversion_preserves_all_fields() {
        // `Into::into` (not `OverrideOperation::from`): the struct's `from`
        // GraphQL field generates an inherent resolver that shadows the
        // trait method.
        let converted: OverrideOperation = OverrideOperationInput {
            op: "replace".to_owned(),
            path: Some("$.model".to_owned()),
            from: None,
            to: None,
            value: Some("gpt-4o".to_owned()),
            condition: None,
            match_: Some(OverrideMatchInput {
                path: "$.stream".to_owned(),
                eq: "true".to_owned(),
            }),
            index: Some(2),
            splat: Some(true),
        }
        .into();
        assert_eq!(converted.op, "replace");
        assert_eq!(converted.path.as_deref(), Some("$.model"));
        assert_eq!(converted.value.as_deref(), Some("gpt-4o"));
        assert_eq!(
            converted.match_,
            Some(OverrideMatch {
                path: "$.stream".to_owned(),
                eq: "true".to_owned(),
            })
        );
        assert_eq!(converted.index, Some(2));
        assert_eq!(converted.splat, Some(true));
    }

    // ---------------------------------------------------------------------
    // Resolver: createChannel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_channel_returns_created_channel_with_ent_defaults() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createChannel(input: {
                        type: openai,
                        name: "prod-openai",
                        credentials: { apiKey: "sk-1" },
                        supportedModels: ["gpt-4o"],
                        defaultTestModel: "gpt-4o",
                        settings: {
                            billingCurrency: " usd ",
                            rechargeMultiplier: "10",
                            proxy: { type: disabled }
                        }
                    }) {
                        id name type status orderingWeight autoSyncSupportedModels supportedModels
                        settings { billingCurrency proxy { type url username password } }
                        defaultEndpoints { apiFormat path baseURL transport }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createChannel"];
        assert_eq!(created["id"], "1");
        assert_eq!(created["name"], "prod-openai");
        assert_eq!(created["type"], "openai");
        // ent column default (schema/channel.go:106): status = disabled.
        assert_eq!(created["status"], "disabled");
        assert_eq!(created["orderingWeight"], 0);
        assert_eq!(created["autoSyncSupportedModels"], false);
        assert_eq!(created["supportedModels"][0], "gpt-4o");
        assert_eq!(created["settings"]["billingCurrency"], "USD");
        assert_eq!(created["settings"]["proxy"]["type"], "disabled");
        assert_eq!(created["settings"]["proxy"]["url"], "");
        assert_eq!(created["settings"]["proxy"]["username"], "");
        assert_eq!(created["settings"]["proxy"]["password"], "");
        assert_eq!(
            created["defaultEndpoints"][0]["apiFormat"],
            "openai/chat_completions"
        );
        assert_eq!(created["defaultEndpoints"][0]["path"], "");
        assert_eq!(created["defaultEndpoints"][0]["baseURL"], "");
        assert_eq!(created["defaultEndpoints"][0]["transport"], "");
        Ok(())
    }

    #[tokio::test]
    async fn create_channel_rejects_incomplete_billing_metadata() {
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createChannel(input: {
                        type: openai,
                        name: "invalid-billing",
                        credentials: {},
                        supportedModels: [],
                        defaultTestModel: "gpt-4o",
                        settings: { billingCurrency: "USD" }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(
            resp.errors[0]
                .message
                .contains("billing_currency and recharge_multiplier must be provided together"),
            "unexpected error: {}",
            resp.errors[0].message
        );
        assert!(lock(&store.channels).is_empty());
    }

    #[tokio::test]
    async fn create_channel_duplicate_name_surfaces_go_error_message() {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(1, "prod-openai"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createChannel(input: {
                        type: openai,
                        name: "prod-openai",
                        credentials: {},
                        supportedModels: [],
                        defaultTestModel: "gpt-4o"
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        // Go xerrors.DuplicateNameError: "%s name '%s' already exists".
        assert!(
            message.contains("channel name 'prod-openai' already exists"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn create_channel_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(
                r#"mutation {
                    createChannel(input: {
                        type: openai,
                        name: "x",
                        credentials: {},
                        supportedModels: [],
                        defaultTestModel: "m"
                    }) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("channel service is not available"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateChannel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_channel_applies_partial_merge() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(7, "old-name"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateChannel(id: "7", input: {
                        name: "new-name",
                        baseURL: "https://api.example.com",
                        remark: "prod"
                    }) { id name baseURL remark defaultTestModel }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updateChannel"];
        assert_eq!(updated["id"], "7");
        assert_eq!(updated["name"], "new-name");
        assert_eq!(updated["baseURL"], "https://api.example.com");
        assert_eq!(updated["remark"], "prod");
        // Unset input fields keep the stored value (partial merge).
        assert_eq!(updated["defaultTestModel"], "gpt-4o");
        Ok(())
    }

    #[tokio::test]
    async fn update_channel_rejects_non_positive_recharge_multiplier() {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(7, "existing"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateChannel(id: "7", input: {
                        settings: {
                            billingCurrency: "USD",
                            rechargeMultiplier: "0"
                        }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(
            resp.errors[0]
                .message
                .contains("recharge_multiplier must be greater than zero"),
            "unexpected error: {}",
            resp.errors[0].message
        );
    }

    #[tokio::test]
    async fn update_channel_clear_flags_follow_go_precedence() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        let mut seeded = sample_channel(3, "c3");
        seeded.remark = Some("keep?".to_owned());
        seeded.auto_sync_model_pattern = Some("^gpt".to_owned());
        lock(&store.channels).push(seeded);
        let schema = schema_with(&store);

        // clearAutoSyncModelPattern wins over a simultaneous set (Go else-if,
        // biz/channel.go ClearAutoSyncModelPattern branch); clearRemark
        // empties remark.
        let resp = schema
            .execute(
                r#"mutation {
                    updateChannel(id: "3", input: {
                        clearRemark: true,
                        autoSyncModelPattern: "^claude",
                        clearAutoSyncModelPattern: true
                    }) { remark autoSyncModelPattern }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateChannel"]["remark"], serde_json::Value::Null);
        assert_eq!(
            data["updateChannel"]["autoSyncModelPattern"],
            serde_json::Value::Null
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_channel_missing_id_surfaces_wrapped_not_found() {
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateChannel(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to update channel: ent: channel not found"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn update_channel_duplicate_name_against_other_channel_errors() {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(1, "alpha"));
        lock(&store.channels).push(sample_channel(2, "beta"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateChannel(id: "2", input: { name: "alpha" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("channel name 'alpha' already exists"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: deleteChannel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn delete_channel_returns_true_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(5, "victim"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteChannel(id: "5") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteChannel"], true);
        assert!(lock(&store.channels).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn delete_channel_missing_id_surfaces_wrapped_error() {
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteChannel(id: "404") }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to delete channel: ent: channel not found"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: channels connection query
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn channels_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(1, "a"));
        lock(&store.channels).push(sample_channel(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    channels {
                        totalCount
                        edges { cursor node { id name } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["channels"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        assert_eq!(connection["pageInfo"]["hasNextPage"], false);
        Ok(())
    }

    #[tokio::test]
    async fn channels_empty_store_returns_empty_connection() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ channels { totalCount edges { cursor } } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["channels"]["totalCount"], 0);
        assert_eq!(data["channels"]["edges"], serde_json::json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn channels_first_limits_page_and_flags_next_page() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        for (id, name) in [(1, "a"), (2, "b"), (3, "c")] {
            lock(&store.channels).push(sample_channel(id, name));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    channels(first: 2) {
                        totalCount
                        edges { node { name } }
                        pageInfo { hasNextPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["channels"]["totalCount"], 3);
        assert_eq!(
            data["channels"]["edges"].as_array().map(std::vec::Vec::len),
            Some(2)
        );
        assert_eq!(data["channels"]["pageInfo"]["hasNextPage"], true);
        Ok(())
    }

    #[tokio::test]
    async fn channels_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        // Go Query.channels (ent.resolvers.go:328-330): CREATED_AT is
        // replaced by ent.DefaultChannelOrder (ID) with direction preserved.
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(1, "a"));
        lock(&store.channels).push(sample_channel(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    channels(orderBy: { field: CREATED_AT, direction: DESC }) {
                        edges { node { id } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].order_by,
            Some(ChannelOrderSelection {
                direction: OrderDirection::Desc,
                term: ChannelOrderTerm::Id,
            })
        );
        // Desc-by-ID ordering is observable in the page.
        let data = resp.data.into_json()?;
        assert_eq!(data["channels"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn channels_order_direction_defaults_to_asc_when_omitted() -> Result<(), TestError> {
        // Contract: `direction: OrderDirection! = ASC` (snapshot line 2276).
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ channels(orderBy: { field: NAME }) { totalCount } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        assert_eq!(
            captured[0].order_by,
            Some(ChannelOrderSelection {
                direction: OrderDirection::Asc,
                term: ChannelOrderTerm::Name,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn channels_passes_where_filter_through_to_service() -> Result<(), TestError> {
        // Go passes `where` straight to ent (`WithChannelFilter`); the Rust
        // resolver must hand the predicate input to the service verbatim.
        let store = InMemoryChannelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    channels(where: {
                        nameContainsFold: "prod",
                        statusIn: [enabled, disabled],
                        baseURLIsNil: true
                    }) { totalCount }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("where filter not captured")?;
        assert_eq!(filter.name_contains_fold.as_deref(), Some("prod"));
        assert_eq!(
            filter.status_in,
            Some(vec![ChannelStatus::Enabled, ChannelStatus::Disabled])
        );
        assert_eq!(filter.base_url_is_nil, Some(true));
        Ok(())
    }

    #[tokio::test]
    async fn channels_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ channels { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("channel service is not available"),
            "unexpected error message: {message}"
        );
    }

    // -----------------------------------------------------------------
    // Edge fields: Channel.requests / Channel.usageLogs
    // (RUST-P12-001 S07 continuation).
    //
    // Mirrors the ent-generated edge resolvers: the connection is scoped
    // to the parent channel id, and the rest of the args pass through
    // unchanged. These tests pin the channel-id injection by inspecting
    // the captured `where_filter` argument the host service received.
    // -----------------------------------------------------------------

    /// Minimal [`crate::request_usage::RequestQueryServices`] double that
    /// captures the args it was called with and returns an empty connection.
    #[derive(Default, Clone)]
    struct FakeRequestServices {
        captured: Arc<Mutex<Vec<crate::request_usage::RequestConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl crate::request_usage::RequestQueryServices for FakeRequestServices {
        async fn requests(
            &self,
            args: crate::request_usage::RequestConnectionArgs,
        ) -> Result<crate::request_usage::RequestConnection, crate::request_usage::RequestQueryError>
        {
            lock(&self.captured).push(args.clone());
            let total_count = args.first.unwrap_or(0) as i64;
            Ok(crate::request_usage::RequestConnection {
                edges: None,
                page_info: crate::pagination::PageInfo {
                    has_next_page: false,
                    has_previous_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count,
            })
        }
    }

    /// Minimal [`crate::request_usage::UsageLogQueryServices`] double.
    #[derive(Default, Clone)]
    struct FakeUsageLogServices {
        captured: Arc<Mutex<Vec<crate::request_usage::UsageLogConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl crate::request_usage::UsageLogQueryServices for FakeUsageLogServices {
        async fn usage_logs(
            &self,
            args: crate::request_usage::UsageLogConnectionArgs,
        ) -> Result<
            crate::request_usage::UsageLogConnection,
            crate::request_usage::UsageLogQueryError,
        > {
            lock(&self.captured).push(args.clone());
            Ok(crate::request_usage::UsageLogConnection {
                edges: None,
                page_info: crate::pagination::PageInfo {
                    has_next_page: false,
                    has_previous_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count: 0,
            })
        }
    }

    fn schema_with_edges(
        channel_store: &InMemoryChannelService,
        requests: &FakeRequestServices,
        usage_logs: &FakeUsageLogServices,
    ) -> AdminSchema {
        let query: Arc<dyn ChannelQueryServices> = Arc::new(channel_store.clone());
        let mutation: Arc<dyn ChannelMutationServices> = Arc::new(channel_store.clone());
        let req: Arc<dyn crate::request_usage::RequestQueryServices> = Arc::new(requests.clone());
        let usage: Arc<dyn crate::request_usage::UsageLogQueryServices> =
            Arc::new(usage_logs.clone());
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(req)
            .data(usage)
            .finish()
    }

    #[tokio::test]
    async fn channel_requests_edge_injects_channel_id_predicate() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(11, "alpha"));
        let requests = FakeRequestServices::default();
        let usage_logs = FakeUsageLogServices::default();
        let schema = schema_with_edges(&store, &requests, &usage_logs);

        let resp = schema
            .execute(
                r#"query {
                    channels {
                        edges {
                            node {
                                requests(first: 5) { totalCount }
                            }
                        }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&requests.captured).clone();
        assert_eq!(captured.len(), 1);
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("requests edge did not pass a where filter")?;
        // ent's edge resolver filters on `channelID = obj.id`; the Rust port
        // mirrors that by overriding the `channel_id` slot with the parent id.
        assert_eq!(filter.channel_id, Some(ID::from("11")));
        assert_eq!(captured[0].first, Some(5));
        Ok(())
    }

    #[tokio::test]
    async fn channel_usage_logs_edge_injects_channel_id_predicate() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(7, "beta"));
        let requests = FakeRequestServices::default();
        let usage_logs = FakeUsageLogServices::default();
        let schema = schema_with_edges(&store, &requests, &usage_logs);

        let resp = schema
            .execute(
                r#"query {
                    channels {
                        edges {
                            node {
                                usageLogs { totalCount }
                            }
                        }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&usage_logs.captured).clone();
        assert_eq!(captured.len(), 1);
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("usageLogs edge did not pass a where filter")?;
        assert_eq!(filter.channel_id, Some(ID::from("7")));
        Ok(())
    }

    #[tokio::test]
    async fn channel_requests_edge_reports_unavailable_when_service_not_wired() {
        // No request service wired — the resolver must surface the same
        // "service is not available" error the rest of the crate uses.
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(1, "solo"));
        let schema = schema_with(&store); // no edge services wired

        let resp = schema
            .execute(
                r#"query {
                    channels { edges { node { requests { totalCount } } } }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("request service is not available"),
            "unexpected error: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // `Channel.executions` edge resolver (RUST-P12-001 S07 continuation).
    // ---------------------------------------------------------------------

    /// Minimal [`crate::request_execution::RequestExecutionQueryServices`]
    /// double that captures args and returns an empty connection.
    #[derive(Default, Clone)]
    struct FakeExecServices {
        captured: Arc<Mutex<Vec<crate::request_execution::RequestExecutionConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl crate::request_execution::RequestExecutionQueryServices for FakeExecServices {
        async fn request_executions(
            &self,
            args: crate::request_execution::RequestExecutionConnectionArgs,
        ) -> Result<
            crate::request_execution::RequestExecutionConnection,
            crate::request_execution::RequestExecutionQueryError,
        > {
            lock(&self.captured).push(args.clone());
            Ok(crate::request_execution::RequestExecutionConnection {
                edges: None,
                page_info: crate::pagination::PageInfo {
                    has_next_page: false,
                    has_previous_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count: 0,
            })
        }
    }

    #[tokio::test]
    async fn channel_executions_edge_injects_channel_id_predicate() -> Result<(), TestError> {
        let store = InMemoryChannelService::default();
        lock(&store.channels).push(sample_channel(33, "gamma"));
        let requests = FakeRequestServices::default();
        let usage_logs = FakeUsageLogServices::default();
        let execs = FakeExecServices::default();

        let query: Arc<dyn ChannelQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ChannelMutationServices> = Arc::new(store.clone());
        let req: Arc<dyn crate::request_usage::RequestQueryServices> = Arc::new(requests.clone());
        let usage: Arc<dyn crate::request_usage::UsageLogQueryServices> =
            Arc::new(usage_logs.clone());
        let exec: Arc<dyn crate::request_execution::RequestExecutionQueryServices> =
            Arc::new(execs.clone());
        let schema = admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(req)
            .data(usage)
            .data(exec)
            .finish();

        let resp = schema
            .execute(
                r#"query {
                    channels { edges { node { executions(first: 2) { totalCount } } } }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&execs.captured).clone();
        assert_eq!(captured.len(), 1);
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("executions edge did not pass a where filter")?;
        // ent's edge resolver filters on `channelID = obj.id`; the Rust port
        // mirrors that by overriding the `channel_id` slot with the parent id.
        assert_eq!(filter.channel_id, Some(ID::from("33")));
        assert_eq!(captured[0].first, Some(2));
        Ok(())
    }
}

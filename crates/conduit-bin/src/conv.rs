//! Row → GraphQL object converters (CONV-01).
//!
//! Bridges the `conduit-db` typed rows (`ModelRow` / `ChannelRow`) to the
//! `conduit-admin-graphql` output objects. The row carries the JSON columns
//! (`model_card` / `settings` / `policies` / `endpoints`) as `serde_json::Value`;
//! the GraphQL objects use typed sub-structs. We bridge through the
//! `conduit-core::objects` types, which are `serde::Deserialize` with the same
//! camelCase field names Ent writes, so `serde_json::from_value` recovers the
//! typed shape and any missing/legacy field degrades to the Go zero value
//! (never a panic) — matching Ent's value-type marshalling.
//!
//! Scope: `model_row_to_gql` (unblocks `bulk_create_models`) and
//! `channel_row_to_gql` (unblocks the connection queries once the association
//! matcher is wired). Both are pure functions with no I/O.

use conduit_admin_graphql::channel::{
    AutoModelMappingRule, CapabilityPolicy, Channel, ChannelEndpoint, ChannelPolicies,
    ChannelRateLimit, ChannelSettings, ChannelStatus, ChannelType, ErrorResponseRewriteRule,
    ModelMapping, OverrideMatch, OverrideOperation, ProxyConfig, ProxyType, RetryableErrorPattern,
    TransformOptions,
};
use conduit_admin_graphql::model::{
    ChannelModelAssociation, ChannelRegexAssociation, ChannelTagsModelAssociation,
    ChannelTagsRegexAssociation, ExcludeAssociation, FilterCondition, FilterConditionType, Model,
    ModelAssociation, ModelAssociationWhen, ModelCard, ModelCardCost, ModelCardLimit,
    ModelCardModalities, ModelCardReasoning, ModelIdAssociation, ModelSettings, ModelStatus,
    ModelType, RegexAssociation,
};
use conduit_admin_graphql::scalars::{AnyScalar, TimeScalar};
use conduit_core::objects as core;
use conduit_db::row::{ChannelRow, ModelRow};
use serde_json::Value;

// ===========================================================================
// Model
// ===========================================================================

/// Convert a `ModelRow` into the GraphQL `Model`. The `model_card` / `settings`
/// JSON columns are deserialized into the core typed objects (zero-filling any
/// missing field, mirroring Ent's value-type marshalling); enum strings map to
/// the GraphQL enums with the Go column defaults as the fallback.
pub fn model_row_to_gql(row: ModelRow) -> Model {
    let card: core::ModelCard = from_value_or_default(row.model_card);
    let settings: core::ModelSettings = from_value_or_default(row.settings);

    Model {
        // Relay Node id — the `gid://conduit/<Type>/<id>` wire form (Go global
        // ids). Mirrors `MeServiceAdapter`'s project id shaping.
        id: format!("gid://conduit/Model/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        developer: row.developer,
        model_id: row.model_id,
        model_type: model_type_from_str(&row.model_type),
        name: row.name,
        icon: row.icon,
        group: row.group_name,
        model_card: model_card_to_gql(card),
        settings: model_settings_to_gql(settings),
        status: model_status_from_str(&row.status),
        remark: row.remark,
        // Create/update payloads need this field immediately. The model CRUD
        // adapter can enrich it when association data is available; a newly
        // created model has no associated channels.
        associated_channel_count: 0,
    }
}

/// Map the model `type` column (Go `ent/model.Type`, default `chat`) to the
/// GraphQL enum. Unknown values fall back to the Go column default `chat`.
fn model_type_from_str(s: &str) -> ModelType {
    match s {
        "embedding" => ModelType::Embedding,
        "rerank" => ModelType::Rerank,
        "image_generation" => ModelType::ImageGeneration,
        "video_generation" => ModelType::VideoGeneration,
        // `chat` and any unexpected value → the Go column default.
        _ => ModelType::Chat,
    }
}

/// Map the model `status` column (Go `ent/model.Status`, default `disabled`)
/// to the GraphQL enum. Unknown values fall back to the Go default `disabled`.
fn model_status_from_str(s: &str) -> ModelStatus {
    match s {
        "enabled" => ModelStatus::Enabled,
        "archived" => ModelStatus::Archived,
        _ => ModelStatus::Disabled,
    }
}

fn model_card_to_gql(card: core::ModelCard) -> ModelCard {
    ModelCard {
        reasoning: ModelCardReasoning {
            supported: card.reasoning.supported,
            default_: card.reasoning.default,
        },
        tool_call: card.tool_call,
        temperature: card.temperature,
        modalities: ModelCardModalities {
            input: card.modalities.input,
            output: card.modalities.output,
        },
        // Go `objects.ModelCard.Vision` is a plain bool (always present on the
        // wire); the GraphQL field is nullable but Go emits the value.
        vision: Some(card.vision),
        cost: ModelCardCost {
            input: card.cost.input,
            output: card.cost.output,
            cache_read: card.cost.cache_read,
            cache_write: card.cost.cache_write,
        },
        limit: ModelCardLimit {
            context: card.limit.context,
            output: card.limit.output,
        },
        // Plain strings in Go — always emitted (empty when unset), never null.
        knowledge: Some(card.knowledge),
        release_date: Some(card.release_date),
        last_updated: Some(card.last_updated),
    }
}

fn model_settings_to_gql(settings: core::ModelSettings) -> ModelSettings {
    ModelSettings {
        disable_developer_settings_inheritance: settings.disable_developer_settings_inheritance,
        associations: settings
            .associations
            .into_iter()
            .map(model_association_to_gql)
            .collect(),
    }
}

pub fn model_association_to_gql(a: core::ModelAssociation) -> ModelAssociation {
    ModelAssociation {
        association_type: a.kind,
        priority: a.priority,
        disabled: a.disabled,
        when: a.when.map(when_to_gql),
        channel_model: a.channel_model.map(|c| ChannelModelAssociation {
            channel_id: c.channel_id,
            model_id: c.model_id,
        }),
        channel_regex: a.channel_regex.map(|c| ChannelRegexAssociation {
            channel_id: c.channel_id,
            pattern: c.pattern,
        }),
        regex: a.regex.map(|r| RegexAssociation {
            pattern: r.pattern,
            exclude: opt_vec(r.exclude.into_iter().map(exclude_to_gql).collect()),
        }),
        model_id: a.model_id.map(|m| ModelIdAssociation {
            model_id: m.model_id,
            exclude: opt_vec(m.exclude.into_iter().map(exclude_to_gql).collect()),
        }),
        channel_tags_model: a.channel_tags_model.map(|c| ChannelTagsModelAssociation {
            channel_tags: c.channel_tags,
            model_id: c.model_id,
        }),
        channel_tags_regex: a.channel_tags_regex.map(|c| ChannelTagsRegexAssociation {
            channel_tags: c.channel_tags,
            pattern: c.pattern,
        }),
    }
}

fn when_to_gql(w: core::ModelAssociationWhen) -> ModelAssociationWhen {
    ModelAssociationWhen {
        enabled: w.enabled,
        condition: w.condition.map(condition_to_gql),
    }
}

fn exclude_to_gql(e: core::ExcludeAssociation) -> ExcludeAssociation {
    ExcludeAssociation {
        // Go `ChannelNamePattern` is a plain string (always emitted). The
        // GraphQL field is nullable; carry the (possibly empty) value.
        channel_name_pattern: Some(e.channel_name_pattern),
        channel_ids: opt_vec(e.channel_ids),
        channel_tags: opt_vec(e.channel_tags),
    }
}

/// Map the recursive core `Condition` tree to the GraphQL `FilterCondition`.
/// Go `objects.Condition.Logic/Field/Operator` are plain strings (always
/// emitted); the type discriminator zero-value (`Omitted`) is classified
/// positionally — a node with children is a group, otherwise a leaf condition
/// (mirrors Go `classify`).
fn condition_to_gql(c: core::Condition) -> FilterCondition {
    let condition_type = match c.r#type {
        core::ConditionType::Group => FilterConditionType::Group,
        core::ConditionType::Condition => FilterConditionType::Condition,
        core::ConditionType::Omitted => {
            if c.conditions.is_empty() {
                FilterConditionType::Condition
            } else {
                FilterConditionType::Group
            }
        }
    };
    FilterCondition {
        condition_type,
        logic: Some(c.logic),
        conditions: opt_vec(c.conditions.into_iter().map(condition_to_gql).collect()),
        field: Some(c.field),
        operator: Some(c.operator),
        value: c.value.map(AnyScalar),
    }
}

// ===========================================================================
// Channel
// ===========================================================================

/// Convert a `ChannelRow` into the GraphQL `Channel`. The `policies` /
/// `settings` / `endpoints` JSON columns are deserialized into the core typed
/// objects; missing/legacy fields degrade to the Go zero value.
///
/// Note: the `credentials` and `disabled_api_keys` columns are intentionally
/// NOT surfaced — the GraphQL `Channel` type has no such fields (credentials are
/// `Sensitive()` in Go and never leave the server through this object).
pub fn channel_row_to_gql(row: ChannelRow) -> Channel {
    let policies: core::channel_settings::ChannelPolicies = from_value_or_default(row.policies);
    let settings: core::channel_settings::ChannelSettings = from_value_or_default(row.settings);
    let endpoints: Vec<core::channel_settings::ChannelEndpoint> = row
        .endpoints
        .into_iter()
        .map(from_value_or_default)
        .collect();

    Channel {
        id: format!("gid://conduit/Channel/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        channel_type: channel_type_from_str(&row.channel_type),
        base_url: row.base_url,
        website_url: row.website_url,
        quota_currency: row.quota_currency.unwrap_or_else(|| "USD".to_string()),
        actual_quota_used: row
            .actual_quota_used
            .and_then(|value| value.parse().ok())
            .map(conduit_admin_graphql::scalars::DecimalScalar),
        quota_remaining: row
            .quota_remaining
            .and_then(|value| value.parse().ok())
            .map(conduit_admin_graphql::scalars::DecimalScalar),
        name: row.name,
        status: channel_status_from_str(&row.status),
        supported_models: row.supported_models,
        manual_models: opt_vec(row.manual_models),
        auto_sync_supported_models: row.auto_sync_supported_models,
        // The retained frontend models this as a string and uses `""` for
        // "no filter". Preserve that usable zero value instead of converting
        // it to GraphQL null.
        auto_sync_model_pattern: Some(row.auto_sync_model_pattern),
        tags: opt_vec(row.tags),
        default_test_model: row.default_test_model,
        policies: Some(channel_policies_to_gql(policies)),
        settings: Some(channel_settings_to_gql(settings)),
        ordering_weight: row.ordering_weight,
        error_message: row.error_message,
        remark: row.remark,
        endpoints: opt_vec(endpoints.into_iter().map(channel_endpoint_to_gql).collect()),
    }
}

fn channel_status_from_str(s: &str) -> ChannelStatus {
    match s {
        "enabled" => ChannelStatus::Enabled,
        "archived" => ChannelStatus::Archived,
        // `disabled` and any unexpected value → the Go column default.
        _ => ChannelStatus::Disabled,
    }
}

fn channel_policies_to_gql(p: core::channel_settings::ChannelPolicies) -> ChannelPolicies {
    ChannelPolicies {
        // Go `ChannelPolicies.Stream` is a plain string (`CapabilityPolicy`,
        // omitempty). Empty/unknown → `None` (the GraphQL field is nullable).
        stream: capability_policy_from_str(&p.stream),
    }
}

/// Map a capability-policy string (`unlimited`/`require`/`forbid`) to the enum.
/// Empty/unknown → `None` (nullable field; a blank column stays absent on the
/// wire rather than being coerced to a default).
fn capability_policy_from_str(s: &str) -> Option<CapabilityPolicy> {
    match s {
        "unlimited" => Some(CapabilityPolicy::Unlimited),
        "require" => Some(CapabilityPolicy::Require),
        "forbid" => Some(CapabilityPolicy::Forbid),
        _ => None,
    }
}

fn channel_settings_to_gql(s: core::channel_settings::ChannelSettings) -> ChannelSettings {
    ChannelSettings {
        management_adapter: s.management_adapter,
        billing_currency: (!s.billing_currency.is_empty()).then_some(s.billing_currency),
        recharge_multiplier: s.recharge_multiplier.map(DecimalScalar),
        // The editor treats an empty prefix as a real zero value, not null.
        extra_model_prefix: Some(s.extra_model_prefix),
        model_mappings: opt_vec(
            s.model_mappings
                .into_iter()
                .map(|m| ModelMapping {
                    from: m.from,
                    to: m.to,
                })
                .collect(),
        ),
        auto_trimed_model_prefixes: opt_vec(s.auto_trimed_model_prefixes),
        hide_original_models: Some(s.hide_original_models),
        hide_mapped_models: Some(s.hide_mapped_models),
        lowercase_model_id: Some(s.lowercase_model_id),
        // Core stores `proxy` as raw JSON (typed ProxyConfig port is pending —
        // see the core `ChannelSettings.proxy` TODO). Decode leniently into the
        // GraphQL `ProxyConfig`; a malformed/absent value degrades to `None`.
        proxy: s.proxy.and_then(proxy_value_to_gql),
        transform_options: Some(TransformOptions {
            force_array_instructions: s.transform_options.force_array_instructions,
            force_array_inputs: s.transform_options.force_array_inputs,
            replace_developer_role_with_system: s
                .transform_options
                .replace_developer_role_with_system,
        }),
        header_override_operations: s
            .header_override_operations
            .into_iter()
            .map(override_op_to_gql)
            .collect(),
        body_override_operations: s
            .body_override_operations
            .into_iter()
            .map(override_op_to_gql)
            .collect(),
        pass_through_user_agent: s.pass_through_user_agent,
        pass_through_body: s.pass_through_body,
        rate_limit: s.rate_limit.map(|r| ChannelRateLimit {
            rpm: r.rpm,
            tpm: r.tpm,
            max_concurrent: r.max_concurrent,
            queue_size: r.queue_size,
            queue_timeout_ms: r.queue_timeout_ms,
        }),
        retryable_status_codes: opt_vec(s.retryable_status_codes),
        retryable_error_patterns: opt_vec(
            s.retryable_error_patterns
                .into_iter()
                .map(|p| RetryableErrorPattern {
                    pattern: p.pattern,
                    regex: p.regex,
                })
                .collect(),
        ),
        auto_model_mapping_rules: opt_vec(
            s.auto_model_mapping_rules
                .into_iter()
                .map(|rule| AutoModelMappingRule {
                    pattern: rule.pattern,
                    public_model_id_template: rule.public_model_id_template,
                    create_draft: rule.create_draft,
                    developer_template: (!rule.developer_template.is_empty())
                        .then_some(rule.developer_template),
                    name_template: (!rule.name_template.is_empty()).then_some(rule.name_template),
                    group_template: (!rule.group_template.is_empty())
                        .then_some(rule.group_template),
                    model_type: rule.model_type,
                })
                .collect(),
        ),
        error_response_rewrite_rules: opt_vec(
            s.error_response_rewrite_rules
                .into_iter()
                .map(|rule| ErrorResponseRewriteRule {
                    status_codes: rule.status_codes.into_iter().map(i64::from).collect(),
                    body_pattern: (!rule.body_pattern.is_empty()).then_some(rule.body_pattern),
                    http_status: rule.http_status.map(i64::from),
                    message: rule.message,
                    error_type: rule.error_type,
                    code: rule.code,
                    body: rule.body.map(async_graphql::Json),
                })
                .collect(),
        ),
    }
}

/// Decode the raw `proxy` JSON column into the GraphQL `ProxyConfig`. The Go
/// `ProxyConfig` uses single-word json tags (`type`/`url`/`username`/
/// `password`); read them defensively and treat a non-object value as absent.
fn proxy_value_to_gql(v: Value) -> Option<ProxyConfig> {
    let obj = v.as_object()?;
    let get_str = |k: &str| obj.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let proxy_type = proxy_type_from_str(get_str("type").as_deref().unwrap_or("DISABLED"));
    Some(ProxyConfig {
        proxy_type,
        url: Some(get_str("url").unwrap_or_default()),
        username: Some(get_str("username").unwrap_or_default()),
        // Passwords are write-only. Returning the persisted value through any
        // Channel projection would let an otherwise read-only nested edge
        // disclose upstream proxy credentials.
        password: Some(String::new()),
    })
}

fn proxy_type_from_str(s: &str) -> ProxyType {
    match s {
        "ENVIRONMENT" => ProxyType::Environment,
        "URL" => ProxyType::Url,
        // `DISABLED` and any unexpected value → the Go default.
        _ => ProxyType::Disabled,
    }
}

fn override_op_to_gql(o: core::overrides::OverrideOperation) -> OverrideOperation {
    // Go `OverrideOperation` string fields are plain (omitempty) values; the
    // GraphQL fields are nullable, so empty → absent.
    OverrideOperation {
        op: o.op,
        path: opt_str(o.path),
        from: opt_str(o.from),
        to: opt_str(o.to),
        value: opt_str(o.value),
        condition: opt_str(o.condition),
        match_: o.r#match.map(|m| OverrideMatch {
            path: m.path,
            eq: m.eq,
        }),
        index: o.index,
        splat: o.splat,
    }
}

fn channel_endpoint_to_gql(e: core::channel_settings::ChannelEndpoint) -> ChannelEndpoint {
    ChannelEndpoint {
        api_format: e.api_format,
        // GraphQL includes explicitly requested nullable fields as `null`,
        // while the retained editor's endpoint schema uses `""` as the
        // absence value. Normalize at this boundary for both defaults and
        // persisted endpoint overrides.
        path: Some(e.path),
        base_url: Some(e.base_url),
        transport: Some(e.transport),
    }
}

fn channel_type_from_str(s: &str) -> ChannelType {
    // The GraphQL `ChannelType` variants are the snake_case Go enum spellings.
    // Map 1:1; an unknown/legacy/typo value falls back to `openai` (the most
    // common default) rather than erroring — a bad type must not black out the
    // whole channel list.
    match s {
        "openai" => ChannelType::Openai,
        "openai_responses" => ChannelType::OpenaiResponses,
        "atlascloud" => ChannelType::Atlascloud,
        "codex" => ChannelType::Codex,
        "vercel" => ChannelType::Vercel,
        "anthropic" => ChannelType::Anthropic,
        "anthropic_aws" => ChannelType::AnthropicAws,
        "anthropic_gcp" => ChannelType::AnthropicGcp,
        "gemini_openai" => ChannelType::GeminiOpenai,
        "gemini" => ChannelType::Gemini,
        "gemini_vertex" => ChannelType::GeminiVertex,
        "deepseek" => ChannelType::Deepseek,
        "deepseek_anthropic" => ChannelType::DeepseekAnthropic,
        "deepinfra" => ChannelType::Deepinfra,
        "qiniu" => ChannelType::Qiniu,
        "fireworks" => ChannelType::Fireworks,
        "doubao" => ChannelType::Doubao,
        "doubao_anthropic" => ChannelType::DoubaoAnthropic,
        "moonshot" => ChannelType::Moonshot,
        "moonshot_anthropic" => ChannelType::MoonshotAnthropic,
        "zhipu" => ChannelType::Zhipu,
        "zai" => ChannelType::Zai,
        "zhipu_anthropic" => ChannelType::ZhipuAnthropic,
        "zai_anthropic" => ChannelType::ZaiAnthropic,
        "anthropic_fake" => ChannelType::AnthropicFake,
        "openai_fake" => ChannelType::OpenaiFake,
        "openrouter" => ChannelType::Openrouter,
        "xiaomi" => ChannelType::Xiaomi,
        "xiaomi_anthropic" => ChannelType::XiaomiAnthropic,
        "xai" => ChannelType::Xai,
        "ppio" => ChannelType::Ppio,
        "siliconflow" => ChannelType::Siliconflow,
        "volcengine" => ChannelType::Volcengine,
        "volcengine_anthropic" => ChannelType::VolcengineAnthropic,
        "longcat" => ChannelType::Longcat,
        "longcat_anthropic" => ChannelType::LongcatAnthropic,
        "minimax" => ChannelType::Minimax,
        "minimax_anthropic" => ChannelType::MinimaxAnthropic,
        "aihubmix" => ChannelType::Aihubmix,
        "aihubmix_anthropic" => ChannelType::AihubmixAnthropic,
        "burncloud" => ChannelType::Burncloud,
        "modelscope" => ChannelType::Modelscope,
        "bailian" => ChannelType::Bailian,
        "bailian_anthropic" => ChannelType::BailianAnthropic,
        "moonshot_coding" => ChannelType::MoonshotCoding,
        "jina" => ChannelType::Jina,
        "github" => ChannelType::Github,
        "github_copilot" => ChannelType::GithubCopilot,
        "claudecode" => ChannelType::Claudecode,
        "cerebras" => ChannelType::Cerebras,
        "antigravity" => ChannelType::Antigravity,
        "nanogpt" => ChannelType::Nanogpt,
        "nanogpt_responses" => ChannelType::NanogptResponses,
        "opencode_go" => ChannelType::OpencodeGo,
        "opencode_go_anthropic" => ChannelType::OpencodeGoAnthropic,
        "ollama" => ChannelType::Ollama,
        "evolink" => ChannelType::Evolink,
        "evolink_anthropic" => ChannelType::EvolinkAnthropic,
        _ => ChannelType::Openai,
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Deserialize a JSON `Value` into `T`, falling back to `T::default()` on any
/// error (missing keys, null, type mismatch). Mirrors Ent's value-type
/// marshalling where an absent/blank JSON column decodes to the Go zero value.
fn from_value_or_default<T: serde::de::DeserializeOwned + Default>(v: Value) -> T {
    serde_json::from_value(v).unwrap_or_default()
}

/// `Vec::is_empty()` → `None`, else `Some(vec)`. GraphQL nullable-list fields
/// omit the key when the underlying slice is empty (Go `omitempty`).
fn opt_vec<T>(v: Vec<T>) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v) }
}

/// Empty string → `None`, else `Some(s)`. Nullable string fields omit the key
/// when blank (Go `omitempty` on the pointer/`*string` field).
fn opt_str(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

// ===========================================================================
// GraphQL input -> core -> JSON Value (for repo writes, e.g. bulk_create_models)
// ===========================================================================

use conduit_admin_graphql::model::{
    ChannelModelAssociationInput, ChannelRegexAssociationInput, ChannelTagsModelAssociationInput,
    ChannelTagsRegexAssociationInput, CreateModelInput as GqlCreateModelInput,
    ExcludeAssociationInput, FilterConditionInput, ModelAssociationInput,
    ModelAssociationWhenInput, ModelCardInput, ModelIdAssociationInput, ModelSettingsInput,
    RegexAssociationInput,
};

/// The JSON `model_card` + `settings` column values plus the resolved `type`
/// string for a `CreateModelInput`, ready to hand to `ModelRepo::create_model`.
/// Mirrors Go `biz.CreateModel` which persists the typed sub-objects as JSON.
pub struct CreateModelColumns {
    /// Model `type` column (Go default `chat` when the input omits it).
    pub model_type: String,
    /// `model_card` JSON column.
    pub model_card: Value,
    /// `settings` JSON column.
    pub settings: Value,
}

/// Build the JSON column values for a GraphQL `CreateModelInput`. The typed
/// card/settings inputs are lowered into the `conduit-core` objects (Go
/// zero-value semantics for omitted fields) and serialized to JSON — the exact
/// shape the row's `#[sqlx(json)]` columns store and that `model_row_to_gql`
/// reads back.
pub fn create_model_columns(input: &GqlCreateModelInput) -> CreateModelColumns {
    let model_type = input
        .model_type
        .map(model_type_to_wire)
        .unwrap_or("chat")
        .to_string();
    let card = model_card_input_to_core(&input.model_card);
    let settings = model_settings_input_to_core(&input.settings);
    CreateModelColumns {
        model_type,
        // Serialization of a plain object cannot fail; fall back to `{}` to stay
        // panic-free regardless.
        model_card: serde_json::to_value(&card)
            .unwrap_or_else(|_| Value::Object(Default::default())),
        settings: serde_json::to_value(&settings)
            .unwrap_or_else(|_| Value::Object(Default::default())),
    }
}

/// GraphQL `ModelType` enum → the Go wire literal (matches the column values).
pub fn model_type_to_wire(t: ModelType) -> &'static str {
    match t {
        ModelType::Chat => "chat",
        ModelType::Embedding => "embedding",
        ModelType::Rerank => "rerank",
        ModelType::ImageGeneration => "image_generation",
        ModelType::VideoGeneration => "video_generation",
    }
}

/// Serialize a GraphQL `ModelCardInput` into the `model_card` JSON column value
/// (the exact shape `model_row_to_gql` reads back). Reuses the same core lowering
/// `create_model_columns` uses so the create and update paths stay identical.
pub fn model_card_input_to_json(input: &ModelCardInput) -> Value {
    let card = model_card_input_to_core(input);
    serde_json::to_value(&card).unwrap_or_else(|_| Value::Object(Default::default()))
}

/// Serialize a GraphQL `ModelSettingsInput` into the `settings` JSON column value.
pub fn model_settings_input_to_json(input: &ModelSettingsInput) -> Value {
    let settings = model_settings_input_to_core(input);
    serde_json::to_value(&settings).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn model_card_input_to_core(input: &ModelCardInput) -> core::ModelCard {
    // Every objects.ModelCard field is a Go value type; omitted inputs become
    // the zero value (mirrors the existing GraphQL `From<ModelCardInput>`).
    core::ModelCard {
        reasoning: core::ModelCardReasoning {
            supported: input
                .reasoning
                .as_ref()
                .and_then(|r| r.supported)
                .unwrap_or(false),
            default: input
                .reasoning
                .as_ref()
                .and_then(|r| r.default_)
                .unwrap_or(false),
        },
        tool_call: input.tool_call.unwrap_or(false),
        temperature: input.temperature.unwrap_or(false),
        modalities: core::ModelCardModalities {
            input: input
                .modalities
                .as_ref()
                .and_then(|m| m.input.clone())
                .unwrap_or_default(),
            output: input
                .modalities
                .as_ref()
                .and_then(|m| m.output.clone())
                .unwrap_or_default(),
        },
        vision: input.vision.unwrap_or(false),
        cost: core::ModelCardCost {
            input: input.cost.as_ref().and_then(|c| c.input).unwrap_or(0.0),
            output: input.cost.as_ref().and_then(|c| c.output).unwrap_or(0.0),
            cache_read: input
                .cost
                .as_ref()
                .and_then(|c| c.cache_read)
                .unwrap_or(0.0),
            cache_write: input
                .cost
                .as_ref()
                .and_then(|c| c.cache_write)
                .unwrap_or(0.0),
        },
        limit: core::ModelCardLimit {
            context: input.limit.as_ref().and_then(|l| l.context).unwrap_or(0),
            output: input.limit.as_ref().and_then(|l| l.output).unwrap_or(0),
        },
        knowledge: input.knowledge.clone().unwrap_or_default(),
        release_date: input.release_date.clone().unwrap_or_default(),
        last_updated: input.last_updated.clone().unwrap_or_default(),
    }
}

fn model_settings_input_to_core(input: &ModelSettingsInput) -> core::ModelSettings {
    core::ModelSettings {
        disable_developer_settings_inheritance: input
            .disable_developer_settings_inheritance
            .unwrap_or(false),
        associations: input
            .associations
            .iter()
            .map(model_association_input_to_core)
            .collect(),
    }
}

pub fn model_association_input_to_core(a: &ModelAssociationInput) -> core::ModelAssociation {
    core::ModelAssociation {
        kind: a.association_type.clone(),
        priority: a.priority.unwrap_or(0),
        disabled: a.disabled.unwrap_or(false),
        when: a.when.as_ref().map(when_input_to_core),
        channel_model: a
            .channel_model
            .as_ref()
            .map(
                |c: &ChannelModelAssociationInput| core::ChannelModelAssociation {
                    channel_id: c.channel_id,
                    model_id: c.model_id.clone(),
                },
            ),
        channel_regex: a
            .channel_regex
            .as_ref()
            .map(
                |c: &ChannelRegexAssociationInput| core::ChannelRegexAssociation {
                    channel_id: c.channel_id,
                    pattern: c.pattern.clone(),
                },
            ),
        regex: a
            .regex
            .as_ref()
            .map(|r: &RegexAssociationInput| core::RegexAssociation {
                pattern: r.pattern.clone(),
                exclude: r
                    .exclude
                    .as_ref()
                    .map(|v| v.iter().map(exclude_input_to_core).collect())
                    .unwrap_or_default(),
            }),
        model_id: a
            .model_id
            .as_ref()
            .map(|m: &ModelIdAssociationInput| core::ModelIDAssociation {
                model_id: m.model_id.clone(),
                exclude: m
                    .exclude
                    .as_ref()
                    .map(|v| v.iter().map(exclude_input_to_core).collect())
                    .unwrap_or_default(),
            }),
        channel_tags_model: a.channel_tags_model.as_ref().map(
            |c: &ChannelTagsModelAssociationInput| core::ChannelTagsModelAssociation {
                channel_tags: c.channel_tags.clone(),
                model_id: c.model_id.clone(),
            },
        ),
        channel_tags_regex: a.channel_tags_regex.as_ref().map(
            |c: &ChannelTagsRegexAssociationInput| core::ChannelTagsRegexAssociation {
                channel_tags: c.channel_tags.clone(),
                pattern: c.pattern.clone(),
            },
        ),
    }
}

fn when_input_to_core(w: &ModelAssociationWhenInput) -> core::ModelAssociationWhen {
    core::ModelAssociationWhen {
        enabled: w.enabled.unwrap_or(false),
        condition: w.condition.as_ref().map(condition_input_to_core),
    }
}

fn exclude_input_to_core(e: &ExcludeAssociationInput) -> core::ExcludeAssociation {
    core::ExcludeAssociation {
        channel_name_pattern: e.channel_name_pattern.clone().unwrap_or_default(),
        channel_ids: e.channel_ids.clone().unwrap_or_default(),
        channel_tags: e.channel_tags.clone().unwrap_or_default(),
    }
}

fn condition_input_to_core(c: &FilterConditionInput) -> core::Condition {
    let r#type = match c.condition_type {
        FilterConditionType::Group => core::ConditionType::Group,
        FilterConditionType::Condition => core::ConditionType::Condition,
    };
    core::Condition {
        r#type,
        logic: c.logic.clone().unwrap_or_default(),
        conditions: c
            .conditions
            .as_ref()
            .map(|v| v.iter().map(condition_input_to_core).collect())
            .unwrap_or_default(),
        field: c.field.clone().unwrap_or_default(),
        operator: c.operator.clone().unwrap_or_default(),
        value: c.value.as_ref().map(|a| a.0.clone()),
    }
}

// ===========================================================================
// GraphQL output → core (reverse of `model_association_to_gql`).
//
// Needed by adapters whose trait hands back *output* shapes (not `*Input`),
// e.g. `SystemSettingsServices::set_model_settings` receives a materialized
// `SystemModelSettings` whose developer associations are GraphQL output
// `ModelAssociation`s that must be persisted through the core typed structs.
// ===========================================================================

/// Map a GraphQL output `ModelAssociation` back to the core typed struct.
/// Exact mirror of [`model_association_to_gql`].
pub fn model_association_to_core(a: ModelAssociation) -> core::ModelAssociation {
    core::ModelAssociation {
        kind: a.association_type,
        priority: a.priority,
        disabled: a.disabled,
        when: a.when.map(when_to_core),
        channel_model: a.channel_model.map(|c| core::ChannelModelAssociation {
            channel_id: c.channel_id,
            model_id: c.model_id,
        }),
        channel_regex: a.channel_regex.map(|c| core::ChannelRegexAssociation {
            channel_id: c.channel_id,
            pattern: c.pattern,
        }),
        regex: a.regex.map(|r| core::RegexAssociation {
            pattern: r.pattern,
            exclude: r
                .exclude
                .map(|v| v.into_iter().map(exclude_to_core).collect())
                .unwrap_or_default(),
        }),
        model_id: a.model_id.map(|m| core::ModelIDAssociation {
            model_id: m.model_id,
            exclude: m
                .exclude
                .map(|v| v.into_iter().map(exclude_to_core).collect())
                .unwrap_or_default(),
        }),
        channel_tags_model: a
            .channel_tags_model
            .map(|c| core::ChannelTagsModelAssociation {
                channel_tags: c.channel_tags,
                model_id: c.model_id,
            }),
        channel_tags_regex: a
            .channel_tags_regex
            .map(|c| core::ChannelTagsRegexAssociation {
                channel_tags: c.channel_tags,
                pattern: c.pattern,
            }),
    }
}

fn when_to_core(w: ModelAssociationWhen) -> core::ModelAssociationWhen {
    core::ModelAssociationWhen {
        enabled: w.enabled,
        condition: w.condition.map(condition_to_core),
    }
}

fn exclude_to_core(e: ExcludeAssociation) -> core::ExcludeAssociation {
    core::ExcludeAssociation {
        channel_name_pattern: e.channel_name_pattern.unwrap_or_default(),
        channel_ids: e.channel_ids.unwrap_or_default(),
        channel_tags: e.channel_tags.unwrap_or_default(),
    }
}

fn condition_to_core(c: FilterCondition) -> core::Condition {
    core::Condition {
        r#type: match c.condition_type {
            FilterConditionType::Group => core::ConditionType::Group,
            FilterConditionType::Condition => core::ConditionType::Condition,
        },
        logic: c.logic.unwrap_or_default(),
        conditions: c
            .conditions
            .map(|v| v.into_iter().map(condition_to_core).collect())
            .unwrap_or_default(),
        field: c.field.unwrap_or_default(),
        operator: c.operator.unwrap_or_default(),
        value: c.value.map(|a| a.0),
    }
}

// ===========================================================================
// ChannelModelPrice / ModelPrice converters (channel-price persistence).
//
// The GraphQL layer uses closed enums (`PricingMode` / `PriceItemCode` /
// `PromptWriteCacheVariantCode`) while the core `objects::pricing` types are
// string-newtypes (open sets). We bridge through the core types so the stored
// JSON column round-trips exactly the way Ent marshals `objects.ModelPrice`.
//
// Mirrors Go `objects.ModelPrice` (`objects/price.go`) and the GraphQL
// input/output shapes in `model_ext.rs`. The GraphQL `PricingMode` enum only
// surfaces three modes (the SDL contract), so an unknown/`usage_volume` stored
// mode maps to `usage_tiered` on output (both share the `usageTiered` shape).
// ===========================================================================

use conduit_admin_graphql::model_ext::PromptWriteCacheVariantCode as GqlVariantCode;
use conduit_admin_graphql::model_ext::{
    ModelPrice as GqlModelPrice, ModelPriceInput, ModelPriceItem as GqlModelPriceItem,
    ModelPriceItemInput, PriceTier as GqlPriceTier, PriceTierInput, Pricing as GqlPricing,
    PricingInput, PricingMode as GqlPricingMode,
    PromptWriteCacheVariant as GqlPromptWriteCacheVariant, PromptWriteCacheVariantInput,
    TieredPricing as GqlTieredPricing, TieredPricingInput,
};
use conduit_admin_graphql::request_usage::PriceItemCode as GqlPriceItemCode;
use conduit_admin_graphql::scalars::DecimalScalar;
use conduit_core::objects::pricing as core_pricing;

// ---- enum <-> wire-string helpers -----------------------------------------

fn pricing_mode_to_wire(mode: GqlPricingMode) -> &'static str {
    match mode {
        GqlPricingMode::FlatFee => core_pricing::PRICING_MODE_FLAT_FEE,
        GqlPricingMode::UsagePerUnit => core_pricing::PRICING_MODE_USAGE_PER_UNIT,
        GqlPricingMode::UsageTiered => core_pricing::PRICING_MODE_TIERED,
    }
}

fn pricing_mode_from_wire(mode: &str) -> GqlPricingMode {
    match mode {
        m if m == core_pricing::PRICING_MODE_FLAT_FEE => GqlPricingMode::FlatFee,
        m if m == core_pricing::PRICING_MODE_USAGE_PER_UNIT => GqlPricingMode::UsagePerUnit,
        // `usage_tiered`, `usage_volume`, and any unknown value share the tiered
        // shape; the SDL enum only exposes `usage_tiered`.
        _ => GqlPricingMode::UsageTiered,
    }
}

fn price_item_code_to_wire(code: GqlPriceItemCode) -> &'static str {
    match code {
        GqlPriceItemCode::PromptTokens => core_pricing::price_item_code::USAGE,
        GqlPriceItemCode::CompletionTokens => core_pricing::price_item_code::COMPLETION,
        GqlPriceItemCode::PromptCachedTokens => core_pricing::price_item_code::PROMPT_CACHED_TOKEN,
        GqlPriceItemCode::PromptWriteCachedTokens => {
            core_pricing::price_item_code::WRITE_CACHED_TOKENS
        }
    }
}

fn price_item_code_from_wire(code: &str) -> GqlPriceItemCode {
    match code {
        c if c == core_pricing::price_item_code::COMPLETION => GqlPriceItemCode::CompletionTokens,
        c if c == core_pricing::price_item_code::PROMPT_CACHED_TOKEN => {
            GqlPriceItemCode::PromptCachedTokens
        }
        c if c == core_pricing::price_item_code::WRITE_CACHED_TOKENS => {
            GqlPriceItemCode::PromptWriteCachedTokens
        }
        // `prompt_tokens` and any unknown value → the usage default.
        _ => GqlPriceItemCode::PromptTokens,
    }
}

fn variant_code_to_wire(code: GqlVariantCode) -> &'static str {
    match code {
        GqlVariantCode::FiveMin => core_pricing::prompt_write_cache_variant_code::FIVE_MIN,
        GqlVariantCode::OneHour => core_pricing::prompt_write_cache_variant_code::ONE_HOUR,
    }
}

fn variant_code_from_wire(code: &str) -> GqlVariantCode {
    match code {
        c if c == core_pricing::prompt_write_cache_variant_code::ONE_HOUR => {
            GqlVariantCode::OneHour
        }
        _ => GqlVariantCode::FiveMin,
    }
}

// ---- GraphQL input -> core -------------------------------------------------

fn pricing_input_to_core(input: PricingInput) -> core_pricing::Pricing {
    core_pricing::Pricing {
        mode: pricing_mode_to_wire(input.mode).to_string(),
        flat_fee: input.flat_fee.map(|d| d.0),
        usage_per_unit: input.usage_per_unit.map(|d| d.0),
        usage_tiered: input.usage_tiered.map(tiered_pricing_input_to_core),
    }
}

fn tiered_pricing_input_to_core(input: TieredPricingInput) -> core_pricing::TieredPricing {
    core_pricing::TieredPricing {
        tiers: input
            .tiers
            .into_iter()
            .map(price_tier_input_to_core)
            .collect(),
    }
}

fn price_tier_input_to_core(input: PriceTierInput) -> core_pricing::PriceTier {
    core_pricing::PriceTier {
        up_to: input.up_to,
        price_per_unit: input.price_per_unit.0,
    }
}

fn variant_input_to_core(
    input: PromptWriteCacheVariantInput,
) -> core_pricing::PromptWriteCacheVariant {
    core_pricing::PromptWriteCacheVariant {
        variant_code: variant_code_to_wire(input.variant_code).to_string(),
        pricing: pricing_input_to_core(input.pricing),
    }
}

fn price_item_input_to_core(input: ModelPriceItemInput) -> core_pricing::ModelPriceItem {
    core_pricing::ModelPriceItem {
        item_code: price_item_code_to_wire(input.item_code).to_string(),
        pricing: pricing_input_to_core(input.pricing),
        prompt_write_cache_variants: input
            .prompt_write_cache_variants
            .unwrap_or_default()
            .into_iter()
            .map(variant_input_to_core)
            .collect(),
    }
}

/// Convert a GraphQL `ModelPriceInput` into the core `objects::ModelPrice`
/// (the shape stored in the `price` JSON column). Mirrors Go's implicit
/// gql→`objects.ModelPrice` unmarshalling.
pub fn model_price_input_to_core(input: ModelPriceInput) -> core_pricing::ModelPrice {
    core_pricing::ModelPrice {
        items: input
            .items
            .into_iter()
            .map(price_item_input_to_core)
            .collect(),
    }
}

// ---- core -> GraphQL output ------------------------------------------------

fn pricing_core_to_gql(p: core_pricing::Pricing) -> GqlPricing {
    GqlPricing {
        mode: pricing_mode_from_wire(&p.mode),
        flat_fee: p.flat_fee.map(DecimalScalar),
        usage_per_unit: p.usage_per_unit.map(DecimalScalar),
        usage_tiered: p.usage_tiered.map(tiered_pricing_core_to_gql),
    }
}

fn tiered_pricing_core_to_gql(t: core_pricing::TieredPricing) -> GqlTieredPricing {
    GqlTieredPricing {
        tiers: t.tiers.into_iter().map(price_tier_core_to_gql).collect(),
    }
}

fn price_tier_core_to_gql(t: core_pricing::PriceTier) -> GqlPriceTier {
    GqlPriceTier {
        up_to: t.up_to,
        price_per_unit: DecimalScalar(t.price_per_unit),
    }
}

fn variant_core_to_gql(v: core_pricing::PromptWriteCacheVariant) -> GqlPromptWriteCacheVariant {
    GqlPromptWriteCacheVariant {
        variant_code: variant_code_from_wire(&v.variant_code),
        pricing: pricing_core_to_gql(v.pricing),
    }
}

fn price_item_core_to_gql(i: core_pricing::ModelPriceItem) -> GqlModelPriceItem {
    // Go `promptWriteCacheVariants` is a nullable list (omitempty): an empty
    // slice serializes as absent, so map empty → None.
    let variants = if i.prompt_write_cache_variants.is_empty() {
        None
    } else {
        Some(
            i.prompt_write_cache_variants
                .into_iter()
                .map(variant_core_to_gql)
                .collect(),
        )
    };
    GqlModelPriceItem {
        item_code: price_item_code_from_wire(&i.item_code),
        pricing: pricing_core_to_gql(i.pricing),
        prompt_write_cache_variants: variants,
    }
}

/// Convert a core `objects::ModelPrice` (read from the `price` JSON column)
/// into the GraphQL `ModelPrice` output object.
pub fn model_price_core_to_gql(price: core_pricing::ModelPrice) -> GqlModelPrice {
    GqlModelPrice {
        items: price
            .items
            .into_iter()
            .map(price_item_core_to_gql)
            .collect(),
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn channel_proxy_password_is_write_only() {
        let proxy = proxy_value_to_gql(serde_json::json!({
            "type": "URL",
            "url": "http://proxy.internal",
            "username": "proxy-user",
            "password": "must-not-leak"
        }))
        .expect("proxy");

        assert_eq!(proxy.url.as_deref(), Some("http://proxy.internal"));
        assert_eq!(proxy.username.as_deref(), Some("proxy-user"));
        assert_eq!(proxy.password.as_deref(), Some(""));
    }
}

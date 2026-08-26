use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

pub type ExtraFields = BTreeMap<String, Value>;

macro_rules! minimal_row {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            pub id: String,
            pub name: String,
            pub project_id: String,
            pub status: String,
            // Placeholder for fields not yet migrated from Ent schema.
            // RUST-P3-002 will replace/expand these structs with table-specific
            // columns, indexes, soft-delete timestamps, and JSON fields.
            #[serde(flatten)]
            pub extra: ExtraFields,
        }

        impl $name {
            pub fn minimal(
                id: impl Into<String>,
                name: impl Into<String>,
                status: impl Into<String>,
            ) -> Self {
                let id = id.into();
                Self {
                    project_id: id.clone(),
                    id,
                    name: name.into(),
                    status: status.into(),
                    extra: ExtraFields::new(),
                }
            }

            pub fn with_project(
                id: impl Into<String>,
                name: impl Into<String>,
                project_id: impl Into<String>,
                status: impl Into<String>,
            ) -> Self {
                Self {
                    id: id.into(),
                    name: name.into(),
                    project_id: project_id.into(),
                    status: status.into(),
                    extra: ExtraFields::new(),
                }
            }

            pub fn json_field<T>(&self, key: &str) -> Result<Option<T>, serde_json::Error>
            where
                T: DeserializeOwned,
            {
                self.extra
                    .get(key)
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
            }

            pub fn set_json_field<T>(
                &mut self,
                key: impl Into<String>,
                value: &T,
            ) -> Result<(), serde_json::Error>
            where
                T: Serialize,
            {
                self.extra.insert(key.into(), serde_json::to_value(value)?);
                Ok(())
            }
        }
    };
}

minimal_row!(BackupRow);

/// Typed row for the `prompt_protection_rules` table. Mirrors the Go
/// `PromptProtectionRule` Ent schema
/// (`conduit/internal/ent/schema/prompt_protection_rule.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/promptprotectionrule.go`).
///
/// RUST-P3-002 S13 batch 3 (stretch): rules are **global** — the Go schema has
/// no `project_id` field/edge (unique index is `(name, deleted_at)`); the
/// fabricated `project_id` from `minimal_row!` is gone. Note the
/// `PromptProtectionRepo` trait still takes a `project_id` scope argument —
/// that is a policy-guard input, not a column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct PromptProtectionRuleRow {
    pub id: String,
    /// Rule name (unique among live rows).
    pub name: String,
    /// Go `Default("")` → plain `String`.
    pub description: String,
    /// Regex pattern to match prompt content (required).
    pub pattern: String,
    /// Go enum `enabled`|`disabled`|`archived`; default `disabled`.
    pub status: String,
    /// Rule settings (Go `*objects.PromptProtectionSettings`, NOT NULL JSON
    /// column — the pointer is a Go marshalling artifact).
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub settings: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `channel_probes` table. Mirrors the Go `ChannelProbe`
/// Ent schema (`conduit/internal/ent/schema/channel_probe.go`; generated
/// struct `conduit/internal/ent/channelprobe.go:16-37`).
///
/// RUST-P3-002 S13 batch 3 (stretch): **no mixin at all** — probes carry
/// neither `created_at`/`updated_at` nor `deleted_at`; the sample instant
/// lives in the immutable `timestamp` column (Go `Int64`, unix). Every field
/// is `Immutable()` in Go — probes are append-only samples.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ChannelProbeRow {
    pub id: String,
    /// Probed channel edge (Go `field.Int("channel_id").Immutable()`).
    pub channel_id: String,
    pub total_request_count: i64,
    pub success_request_count: i64,
    /// Go `field.Float(...).Optional().Nillable()` → `Option<f64>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_tokens_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_time_to_first_token_ms: Option<f64>,
    /// Sample instant, unix seconds (Go `field.Int64("timestamp")`).
    pub timestamp: i64,
}

/// Typed row for the `api_key_profile_templates` table. Mirrors the Go
/// `APIKeyProfileTemplate` Ent schema
/// (`conduit/internal/ent/schema/api_key_profile_template.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/apikeyprofiletemplate.go`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ApiKeyProfileTemplateRow {
    pub id: String,
    /// Template name.
    pub name: String,
    /// Go `Default("")` → plain `String`.
    pub description: String,
    /// Owning project edge (Go `field.Int("project_id").Immutable()`).
    pub project_id: String,
    /// Go `*objects.APIKeyProfile`, `Optional()` + `Default(&{})` → nullable
    /// JSON column → `Option<Value>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json)]
    pub profile: Option<Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `channel_model_prices` table. Mirrors the Go
/// `ChannelModelPrice` Ent schema
/// (`conduit/internal/ent/schema/channel_model_price.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/channelmodelprice.go`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ChannelModelPriceRow {
    pub id: String,
    /// Priced channel edge (Go `field.Int("channel_id").Immutable()`).
    pub channel_id: String,
    /// Provider model id (Go `field.String("model_id").Immutable()`).
    pub model_id: String,
    /// Real-world accounting currency for the current import price.
    pub currency_code: String,
    /// Current price document (Go `objects.ModelPrice`, NOT NULL JSON).
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub price: Value,
    /// Billing reference id (Go `Unique()`); regenerated when price changes.
    pub reference_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `channel_model_price_versions` table. Mirrors the Go
/// `ChannelModelPriceVersion` Ent schema
/// (`conduit/internal/ent/schema/channel_model_price_versions.go` +
/// TimeMixin; generated struct
/// `conduit/internal/ent/channelmodelpriceversion.go:17-45`).
///
/// **No `deleted_at`** — TimeMixin only (price versions are an immutable
/// history; the head row `ChannelModelPrice` soft-deletes instead).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ChannelModelPriceVersionRow {
    pub id: String,
    pub channel_id: String,
    pub model_id: String,
    /// Head-row edge (Go `field.Int("channel_model_price_id").Immutable()`).
    pub channel_model_price_id: String,
    /// Immutable real-world accounting currency for this price version.
    pub currency_code: String,
    /// Versioned price document (Go `objects.ModelPrice`, Immutable JSON).
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub price: Value,
    /// Go enum `active`|`archived`.
    pub status: String,
    /// Effective start (Go: required, Immutable).
    pub effective_start_at: chrono::DateTime<chrono::Utc>,
    /// Effective end; `None` = effective until the next version
    /// (Go `*time.Time`, Optional+Nillable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_end_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Billing reference id (Go `Unique().Immutable()`).
    pub reference_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `channel_override_templates` table. Mirrors the Go
/// `ChannelOverrideTemplate` Ent schema
/// (`conduit/internal/ent/schema/channel_override_template.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/channeloverridetemplate.go:16-50`).
///
/// The two deprecated columns (`override_parameters`, `override_headers`) are
/// kept — they are still real Go columns with defaults; dropping them here
/// would drift from the DB schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ChannelOverrideTemplateRow {
    pub id: String,
    /// Owning user edge (Go `field.Int("user_id").Optional().Immutable()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Template name, unique per user (Go `NotEmpty()`).
    pub name: String,
    /// Go `Optional()` (no default) → `Option<String>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Deprecated in Go ("Use body_override_operations instead"); JSON text
    /// stored as a plain string column, default `"{}"`.
    #[serde(default = "default_override_parameters")]
    pub override_parameters: String,
    /// Deprecated in Go ("Use header_override_operations instead"); JSON list
    /// of `objects.HeaderEntry`, default `[]`.
    #[serde(default = "default_json_array")]
    #[sqlx(json)]
    pub override_headers: Value,
    /// JSON list of `objects.OverrideOperation`; Go `Optional()` + default
    /// `[]` with a non-pointer Go field (NULL scans to the empty list).
    #[serde(default = "default_json_array")]
    #[sqlx(json)]
    pub header_override_operations: Value,
    #[serde(default = "default_json_array")]
    #[sqlx(json)]
    pub body_override_operations: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Go `field.String("override_parameters").DefaultFunc(... "{}")`.
fn default_override_parameters() -> String {
    "{}".to_string()
}

/// Go `field.JSON(..., []objects.X{}).Default([]objects.X{})`.
fn default_json_array() -> Value {
    Value::Array(Vec::new())
}

/// Typed row for the `systems` table. Mirrors the Go `System` Ent schema
/// (`conduit/internal/ent/schema/system.go` + TimeMixin + SoftDeleteMixin;
/// generated struct `conduit/internal/ent/system.go:16-31`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// The fabricated `name`/`project_id`/`status` columns are gone — the Go
/// `System` has only `key` (unique) + `value` besides the mixin columns.
/// `value` is a plain string column (Go stores raw text for string settings
/// and `json.Marshal` output for structured values).
///
/// Go `SoftDeleteMixin` stores `deleted_at` as an INTEGER (`0` = live, unix
/// seconds when deleted — `schematype/soft_delete.go`); the SQL layer NULLIFs
/// it into `Option<DateTime>` like `UserRow`/`ProjectRow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct SystemRow {
    /// Stringified DB integer PK (matches `UserRow.id` convention).
    pub id: String,
    /// Unique settings key (Go: `field.String("key").Unique()`).
    pub key: String,
    /// Free-form setting value (Go: plain `string`).
    pub value: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `request_executions` table. Mirrors the Go
/// `RequestExecution` Ent schema
/// (`conduit/internal/ent/schema/request_execution.go` + TimeMixin; generated
/// struct `conduit/internal/ent/requestexecution.go:21-73`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** — TimeMixin only. The fabricated `name` is dropped.
/// Edge INTEGER columns are stringified (`project_id`, `request_id` NOT NULL →
/// `String`; `channel_id`, `data_storage_id` `Optional()` → `Option<String>`),
/// matching `RequestRow`. JSON columns (`request_body` NOT NULL;
/// `response_body`/`response_chunks`/`request_headers` `Optional()`) are
/// `serde_json::Value` — `response_chunks` is a JSON array of raw chunks
/// (Go `[]objects.JSONRawMessage`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct RequestExecutionRow {
    pub id: String,
    /// Go `field.Int("project_id").Immutable().Default(1)`.
    pub project_id: String,
    /// Parent request edge (Go `field.Int("request_id").Immutable()`).
    pub request_id: String,
    /// Optional because the channel may have been deleted (schema comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    /// Stable one-way identity of the credential actually selected for this
    /// attempt. Never stores the provider secret itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_storage_id: Option<String>,
    /// External tracking id (Go `Optional().MaxLen(512)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    pub model_id: String,
    /// Go default `openai/chat_completions`.
    pub format: String,
    /// Provider-shaped request body (Go JSON, Immutable, NOT NULL).
    #[sqlx(json)]
    pub request_body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub response_body: Option<Value>,
    /// JSON array of streaming chunks (Go `[]objects.JSONRawMessage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub response_chunks: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// HTTP status from the upstream provider (Go `*int`, Optional+Nillable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status_code: Option<i64>,
    /// Go enum `pending`|`processing`|`completed`|`failed`|`canceled`.
    pub status: String,
    /// Go `Default(false).Immutable()`.
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_first_token_latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_reasoning_duration_ms: Option<i64>,
    /// Masked request headers (Go JSON, Optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub request_headers: Option<Value>,
    /// Actual upstream URL (Go `Optional()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_url: Option<String>,
    /// Go `Default(false)`.
    pub pass_through_applied: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Successful route feedback for a hashed explicit provider cache/continuity
/// key. Raw cache keys, response ids, prompts, and credentials are deliberately
/// absent from this row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct RouteAffinityRow {
    pub id: String,
    pub project_id: String,
    pub key_class: String,
    pub key_hash: String,
    pub public_model_id: String,
    pub api_format: String,
    pub channel_id: String,
    pub upstream_model_id: String,
    pub upstream_api_format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_identity: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `user_projects` join table. Mirrors the Go `UserProject`
/// Ent schema (`conduit/internal/ent/schema/user_project.go` + TimeMixin;
/// generated struct `conduit/internal/ent/userproject.go:19-39`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** — TimeMixin only. No fabricated `name`/`status`.
/// `(user_id, project_id)` is unique (`user_projects_by_user_id_project_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct UserProjectRow {
    pub id: String,
    /// Go `field.Int("user_id").Immutable()` (edge to User).
    pub user_id: String,
    /// Go `field.Int("project_id").Immutable()` (edge to Project).
    pub project_id: String,
    /// Go `Default(false)`; mutable to allow ownership transfer.
    pub is_owner: bool,
    /// Per-user project scopes (Go `field.Strings` default `[]`, Optional).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `user_roles` join table. Mirrors the Go `UserRole` Ent
/// schema (`conduit/internal/ent/schema/user_role.go`; generated struct
/// `conduit/internal/ent/userrole.go:18-34`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// **No mixin at all** — `UserRole.Mixin()` returns an empty slice; the
/// timestamps are declared inline as `Optional().Nillable()` ("nullable for
/// compatibility with old data"), so both are `Option<DateTime>` here (Go
/// `*time.Time`). `(user_id, role_id)` is unique
/// (`user_roles_by_user_id_role_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct UserRoleRow {
    pub id: String,
    /// Go `field.Int("user_id").Immutable()` (edge to User).
    pub user_id: String,
    /// Go `field.Int("role_id").Immutable()` (edge to Role).
    pub role_id: String,
    /// Nullable for legacy rows (Go `*time.Time`, Optional+Nillable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `oidc_identities` table. Mirrors the Go `OIDCIdentity`
/// Ent schema (`conduit/internal/ent/schema/oidc_identity.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/oidcidentity.go:17-43`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// No fabricated `name`/`project_id`/`status`. `(issuer, subject, deleted_at)`
/// is unique (`oidc_identities_by_issuer_subject_deleted_at`); `user_id` is a
/// required edge to User (cascade delete).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct OidcIdentityRow {
    pub id: String,
    /// OIDC provider issuer URL (Go: required string).
    pub issuer: String,
    /// OIDC subject identifier (Go: required string).
    pub subject: String,
    /// Email from the OIDC provider (Go `Optional()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Identity provider name (Go `Optional()`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idp_name: Option<String>,
    /// Last login timestamp (Go `*time.Time`, Optional+Nillable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Owning user edge (Go `field.Int("user_id")`, required).
    pub user_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `provider_quota_statuses` table. Mirrors the Go
/// `ProviderQuotaStatus` Ent schema
/// (`conduit/internal/ent/schema/provider_quota_status.go` + TimeMixin +
/// SoftDeleteMixin; generated struct
/// `conduit/internal/ent/providerquotastatus.go:18-46`).
///
/// RUST-P3-002 S13 batch 3: hand-written typed struct replacing `minimal_row!`.
/// `channel_id` is a unique-indexed required edge (one status row per
/// channel). `quota_data` is a required JSON object (Go `map[string]any`) kept
/// as `serde_json::Value` — the repo layer owns serde.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ProviderQuotaStatusRow {
    pub id: String,
    /// Owning channel edge (Go `field.Int("channel_id").Immutable()`; unique).
    pub channel_id: String,
    /// Go enum `claudecode`|`codex`|`github_copilot`|`nanogpt`|`wafer`|
    /// `synthetic`|`neuralwatt`|`apertis` (Immutable).
    pub provider_type: String,
    /// Go enum `available`|`warning`|`exhausted`|`unknown`.
    pub status: String,
    /// Provider-specific quota data (Go `field.JSON(map[string]any)`).
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub quota_data: Value,
    /// Next quota reset (Go `*time.Time`, Optional+Nillable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_reset_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Go `Default(true)` — true when status is available or warning.
    pub ready: bool,
    /// Next scheduled quota check (Go: required time).
    pub next_check_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted (Go INTEGER `deleted_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `users` table. Mirrors the Go `User` Ent schema
/// (`conduit/internal/ent/schema/user.go:37-56` + mixins).
///
/// RUST-P3-002 S13 pilot: the first hand-written row struct, replacing the
/// generic `minimal_row!` shape. Each field maps 1:1 to a real Go entity
/// column. `password` is intentionally absent — it stays in the auth layer
/// (exposing it through a read repo would be a credential leak). The
/// previously synthesized `name`/`project_id` columns (fabricated by the
/// macro) are gone — `User` has neither.
///
/// This struct derives `sqlx::FromRow` so the PostgreSQL repository can use
/// `query_as::<_, UserRow>` directly (the `scopes` JSON
/// TEXT column is decoded via `#[sqlx(json)]`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct UserRow {
    /// Stringified DB integer PK (InMemory keys by caller-supplied string;
    /// Repository queries stringify the database id). Keeping `String` avoids
    /// rippling the InMemory API to `i64`.
    pub id: String,
    pub email: String,
    /// Go enum `activated`|`deactivated`. Kept as `String` for now; a typed
    /// enum will land in a later step.
    pub status: String,
    /// Go default `"en"`.
    pub prefer_language: String,
    pub first_name: String,
    pub last_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub is_owner: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `None` = live; `Some(ts)` = soft-deleted. Mirrors Go `SoftDeleteMixin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `channels` table. Mirrors the Go `Channel` Ent schema
/// (`conduit/internal/ent/schema/channel.go:35-155` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `project_id` is dropped — channels are global (not project-scoped).
/// JSON columns (`credentials`, `disabled_api_keys`, `policies`, `settings`,
/// `endpoints`) are `serde_json::Value` / `Vec<Value>` (repo owns serde).
/// `credentials` is sensitive (Go: `Sensitive()`) — kept on the row because the
/// repo needs to pass it through; callers are responsible for not leaking it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ChannelRow {
    pub id: String,
    /// Go enum stored as `String` (`"type"` column; `type` is a SQL reserved word).
    #[sqlx(rename = "type")]
    pub channel_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_currency: Option<String>,
    /// Exact decimal string copied from the provider billing console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_quota_used: Option<String>,
    /// Exact decimal string copied from the provider billing console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_remaining: Option<String>,
    pub name: String,
    /// Go enum `enabled`|`disabled`|`archived`; default `disabled`.
    pub status: String,
    pub credentials: Value,
    #[serde(default = "Value::default", skip_serializing_if = "Value::is_null")]
    pub disabled_api_keys: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub supported_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub manual_models: Vec<String>,
    pub auto_sync_supported_models: bool,
    /// Go: `Optional().Default("")` — stored as `String` (empty = no filter).
    pub auto_sync_model_pattern: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub tags: Vec<String>,
    pub default_test_model: String,
    /// Go default `{"stream": "unlimited"}`.
    #[serde(default = "default_policies", skip_serializing_if = "Value::is_null")]
    #[sqlx(json)]
    pub policies: Value,
    /// Go default `{"model_mappings": []}`.
    #[serde(default = "default_settings", skip_serializing_if = "Value::is_null")]
    #[sqlx(json)]
    pub settings: Value,
    pub ordering_weight: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub endpoints: Vec<Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn default_policies() -> Value {
    serde_json::json!({"stream": "unlimited"})
}

fn default_settings() -> Value {
    serde_json::json!({"model_mappings": []})
}

/// Typed row for the `projects` table. Mirrors the Go `Project` Ent schema
/// (`conduit/internal/ent/schema/project.go:37-54` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `project_id` is dropped — the Go `Project` has no such field (the project
/// IS the project). `profiles` is `serde_json::Value` (repo owns serde).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    /// Go enum `active`|`archived`; default `active`.
    pub status: String,
    pub description: String,
    /// Go default `{}` (empty `objects.ProjectProfiles`).
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub profiles: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `api_keys` table. Mirrors the Go `APIKey` Ent schema
/// (`conduit/internal/ent/schema/api_key.go:39-77` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `project_id` is kept (Go has it as a real column). `key` is the unique
/// business key (single-column unique index — spans all rows including
/// soft-deleted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: String,
    pub name: String,
    pub status: String,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub key: String,
    /// Go enum `user`|`service_account`|`noauth` (`"type"` column; reserved word).
    #[sqlx(rename = "type")]
    pub key_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub scopes: Vec<String>,
    /// Go default `{}`.
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub profiles: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `models` table. Mirrors the Go `Model` Ent schema
/// (`conduit/internal/ent/schema/model.go:37-56` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `project_id` dropped (models are global). `model_card`/`settings` are
/// `serde_json::Value` (repo owns serde). `developer`/`icon`/`group` are
/// NOT NULL in the DB schema (Go has no Optional on them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ModelRow {
    pub id: String,
    pub name: String,
    pub status: String,
    pub developer: String,
    /// Provider model id (e.g. `"deepseek-chat"`).
    pub model_id: String,
    /// Go enum `chat`|`embedding`|`rerank`|... (`"type"` column; reserved word).
    #[sqlx(rename = "type")]
    pub model_type: String,
    pub icon: String,
    /// `"group"` column; SQL reserved word.
    #[sqlx(rename = "group")]
    pub group_name: String,
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub model_card: Value,
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub settings: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remark: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `data_storages` table. Mirrors the Go `DataStorage` Ent
/// schema (`conduit/internal/ent/schema/data_storage.go:37-61` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `project_id` dropped (data storages are global). `settings` is
/// `serde_json::Value` (repo owns serde). `primary` is `Immutable()` in Go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct DataStorageRow {
    pub id: String,
    pub name: String,
    pub status: String,
    pub description: String,
    /// Go: `Immutable().Default(false)`. `"primary"` is a SQL reserved word.
    #[sqlx(rename = "primary")]
    pub primary: bool,
    /// Go enum `database|fs|s3|gcs|webdav` (`"type"` column; reserved word).
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]
    pub storage_type: String,
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub settings: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `prompts` table. Mirrors the Go `Prompt` Ent schema
/// (`conduit/internal/ent/schema/prompt.go:40-67` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `order` is a SQL reserved word (Rust field `order_val` with rename).
/// `settings` is `serde_json::Value` (repo owns serde).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct PromptRow {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub status: String,
    pub description: String,
    pub role: String,
    pub content: String,
    /// Go: `field.Int("order")` — `"order"` is a SQL reserved word.
    #[serde(rename = "order")]
    #[sqlx(rename = "order")]
    pub order_val: i64,
    #[serde(default = "Value::default")]
    #[sqlx(json)]
    pub settings: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `roles` table. Mirrors the Go `Role` Ent schema
/// (`conduit/internal/ent/schema/role.go:38-57` + mixins).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// `status` is NOT a Go column — it's an application-layer convenience
/// ("active"/"deactivated") kept for InMemory parity. The Go schema uses
/// `level` + `deleted_at` to track lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct RoleRow {
    pub id: String,
    pub name: String,
    /// Go enum `system`|`project` (Immutable, default `system`).
    pub level: String,
    /// `""` for system roles (Go: NULL); project id for project roles.
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[sqlx(json)]
    pub scopes: Vec<String>,
    /// Application-layer "active"/"deactivated" (not a Go column).
    pub status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Typed row for the `requests` table. Mirrors the Go `Request` Ent schema
/// (`conduit/internal/ent/schema/request.go:41-123` + TimeMixin).
///
/// RUST-P3-002 S13: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** — the Go `Request` schema uses `TimeMixin` only (no
/// `SoftDeleteMixin`). `name`/`project_id` fabricated by `minimal_row!` are
/// dropped (`name`) or kept as real Go columns (`project_id`).
///
/// `"source"` and `"status"` are quoted in the DDL (SQL reserved words) but
/// PostgreSQL returns them using selected aliases, so no `#[sqlx(rename)]` is needed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct RequestRow {
    pub id: String,
    pub project_id: String,
    /// Go enum `pending`|`processing`|`completed`|`failed`|`canceled`.
    pub status: String,
    /// Go enum `api`|`playground`|`test` (`"source"` column; Immutable).
    pub source: String,
    pub model_id: String,
    pub format: String,
    pub stream: bool,
    pub client_ip: String,
    pub content_saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_storage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub request_headers: Option<Value>,
    #[sqlx(json)]
    pub request_body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub response_body: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[sqlx(json(nullable))]
    pub response_chunks: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_first_token_latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_reasoning_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_storage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_saved_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `traces` table. Mirrors the Go `Trace` Ent schema
/// (`conduit/internal/ent/schema/trace.go:38-50` + TimeMixin; generated struct
/// `conduit/internal/ent/trace.go:18-36`).
///
/// RUST-P3-002 S13 batch 2: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** (TimeMixin only) and no fabricated `name`/`status`
/// columns — the Go `Trace` has neither. `thread_id` is `Optional()` +
/// `Immutable()` in Go (nullable INTEGER edge) → `Option<String>` (edge ids are
/// stringified, matching `RequestRow.trace_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct TraceRow {
    pub id: String,
    pub project_id: String,
    /// Unique trace identifier (Go: global unique index `traces_by_trace_id`).
    pub trace_id: String,
    /// Owning thread edge; `None` when the trace is threadless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `threads` table. Mirrors the Go `Thread` Ent schema
/// (`conduit/internal/ent/schema/thread.go:36-45` + TimeMixin; generated
/// struct `conduit/internal/ent/thread.go:16-33`).
///
/// RUST-P3-002 S13 batch 2: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** (TimeMixin only) and no fabricated `name`/`status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct ThreadRow {
    pub id: String,
    pub project_id: String,
    /// Unique thread identifier (Go: global unique index `threads_by_thread_id`).
    pub thread_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Typed row for the `usage_logs` table. Mirrors the Go `UsageLog` Ent schema
/// (`conduit/internal/ent/schema/usage_log.go:43-90` + TimeMixin; generated
/// struct `conduit/internal/ent/usagelog.go:20-77`).
///
/// RUST-P3-002 S13 batch 2: hand-written typed struct replacing `minimal_row!`.
/// **No `deleted_at`** (TimeMixin only); the fabricated `name`/`status` are
/// dropped. Edge INTEGER columns are stringified (`request_id`, `project_id`
/// NOT NULL → `String`; `api_key_id`, `channel_id` `Optional()` →
/// `Option<String>`).
///
/// The nine token-breakdown counters are Go `Int64().Default(0).Optional()`
/// with a non-pointer Go struct field — Ent scans NULL to `0` — so they are
/// plain `i64` here; repository SELECTs apply `COALESCE(x, 0)`.
/// `total_cost` is `Nillable()` → `Option<f64>`. `cost_items` is a JSON column
/// (Go `[]objects.CostItem`, default `[]`) kept as `serde_json::Value` — the
/// repo layer owns serde. `cost_price_reference_id` is `Optional()` →
/// `Option<String>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(sqlx::FromRow)]
pub struct UsageLogRow {
    pub id: String,
    /// Parent request edge (Go: `field.Int("request_id").Immutable()`).
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_id: Option<String>,
    /// Go default `1` (backward compatibility).
    pub project_id: String,
    /// Optional because the channel may have been deleted (schema comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    pub model_id: String,
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub prompt_audio_tokens: i64,
    #[serde(default)]
    pub prompt_cached_tokens: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens_5m: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens_1h: i64,
    #[serde(default)]
    pub completion_audio_tokens: i64,
    #[serde(default)]
    pub completion_reasoning_tokens: i64,
    #[serde(default)]
    pub completion_accepted_prediction_tokens: i64,
    #[serde(default)]
    pub completion_rejected_prediction_tokens: i64,
    /// Go enum `api`|`playground`|`test`; default `api` (`"source"` column is
    /// quoted in DDL but selected without the quotes — no `sqlx(rename)`).
    pub source: String,
    /// Go default `openai/chat_completions`.
    pub format: String,
    /// Go `field.Float().Nillable().Optional()` — `None` = cost not computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost: Option<f64>,
    /// JSON array of `objects.CostItem`; Go default `[]`.
    #[serde(default = "default_cost_items")]
    #[sqlx(json)]
    pub cost_items: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_price_reference_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Go `field.JSON("cost_items", []objects.CostItem{}).Default([]...)`.
fn default_cost_items() -> Value {
    Value::Array(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn row_json_round_trip_preserves_unknown_fields() -> Result<(), serde_json::Error> {
        // BackupRow is the macro exemplar now — it has no Go Ent schema yet, so
        // it stays on the generic `minimal_row!` shape (S13 batch 3 converted
        // System/RequestExecution/UserProject/UserRole/OidcIdentity/
        // ProviderQuotaStatus to typed structs).
        let input = json!({
            "id": "bk-1",
            "name": "nightly",
            "project_id": "bk-1",
            "status": "completed",
            "value": true,
            "metadata": {"source": "test"}
        });

        let row: BackupRow = serde_json::from_value(input)?;
        let output = serde_json::to_value(row)?;

        assert_eq!(output["value"], true);
        assert_eq!(output["metadata"]["source"], "test");
        Ok(())
    }

    #[test]
    fn json_field_helpers_round_trip_typed_values() -> Result<(), serde_json::Error> {
        let mut row = BackupRow::minimal("bk-1", "nightly", "completed");
        row.set_json_field("usage", &json!({"input_tokens": 10, "extra": "keep"}))?;

        let usage: Option<Value> = row.json_field("usage")?;
        assert_eq!(usage, Some(json!({"input_tokens": 10, "extra": "keep"})));

        let output = serde_json::to_value(row)?;
        assert_eq!(output["usage"]["extra"], "keep");
        Ok(())
    }

    // --- RUST-P3-002 S13 batch 3: typed row serde shape checks --------------

    fn ts(s: &str) -> Result<chrono::DateTime<chrono::Utc>, chrono::ParseError> {
        Ok(chrono::DateTime::parse_from_rfc3339(s)?.with_timezone(&chrono::Utc))
    }

    /// `SystemRow` carries the Go `System` columns (key/value + mixins) and no
    /// fabricated `name`/`project_id`/`status`.
    #[test]
    fn system_row_serde_camel_case_shape() -> Result<(), Box<dyn std::error::Error>> {
        let row = SystemRow {
            id: "1".into(),
            key: "brand_name".into(),
            value: "Conduit API".into(),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&row)?;
        assert_eq!(v["key"], "brand_name");
        assert_eq!(v["value"], "Conduit API");
        assert_eq!(v["createdAt"], "2024-01-01T00:00:00Z");
        // Live row: deleted_at is skipped entirely.
        assert!(v.as_object().and_then(|o| o.get("deletedAt")).is_none());
        // No fabricated legacy columns survive on the typed row.
        assert!(v.as_object().and_then(|o| o.get("name")).is_none());
        assert!(v.as_object().and_then(|o| o.get("status")).is_none());
        let back: SystemRow = serde_json::from_value(v)?;
        assert_eq!(back, row);
        Ok(())
    }

    /// `UserProjectRow`: TimeMixin only (no deletedAt); scopes JSON list.
    #[test]
    fn user_project_row_serde_shape() -> Result<(), Box<dyn std::error::Error>> {
        let row = UserProjectRow {
            id: "1".into(),
            user_id: "7".into(),
            project_id: "3".into(),
            is_owner: true,
            scopes: vec!["read_channels".into(), "write_channels".into()],
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-02T00:00:00Z")?,
        };
        let v = serde_json::to_value(&row)?;
        assert_eq!(v["userId"], "7");
        assert_eq!(v["projectId"], "3");
        assert_eq!(v["isOwner"], true);
        assert_eq!(v["scopes"], json!(["read_channels", "write_channels"]));
        assert!(v.as_object().and_then(|o| o.get("deletedAt")).is_none());
        let back: UserProjectRow = serde_json::from_value(v)?;
        assert_eq!(back, row);
        Ok(())
    }

    /// `UserRoleRow`: no mixin — both timestamps are nullable (legacy rows).
    #[test]
    fn user_role_row_nullable_timestamps() -> Result<(), Box<dyn std::error::Error>> {
        // Legacy row shape: created_at/updated_at absent entirely.
        let legacy: UserRoleRow = serde_json::from_value(json!({
            "id": "1", "userId": "7", "roleId": "2"
        }))?;
        assert_eq!(legacy.created_at, None);
        assert_eq!(legacy.updated_at, None);
        let v = serde_json::to_value(&legacy)?;
        assert!(v.as_object().and_then(|o| o.get("createdAt")).is_none());

        // Modern row round-trips the timestamps.
        let modern = UserRoleRow {
            id: "2".into(),
            user_id: "7".into(),
            role_id: "9".into(),
            created_at: Some(ts("2024-01-01T00:00:00Z")?),
            updated_at: Some(ts("2024-01-01T00:00:00Z")?),
        };
        let back: UserRoleRow = serde_json::from_value(serde_json::to_value(&modern)?)?;
        assert_eq!(back, modern);
        Ok(())
    }

    /// `RequestExecutionRow`: optional JSON/metric fields are skipped when
    /// unset and round-trip when present.
    #[test]
    fn request_execution_row_serde_shape() -> Result<(), Box<dyn std::error::Error>> {
        let row = RequestExecutionRow {
            id: "1".into(),
            project_id: "1".into(),
            request_id: "5".into(),
            channel_id: Some("3".into()),
            credential_identity: Some("sha256:test".into()),
            data_storage_id: None,
            external_id: None,
            model_id: "claude-3-5-sonnet".into(),
            format: "claude/messages".into(),
            request_body: json!({"model": "claude-3-5-sonnet"}),
            response_body: None,
            response_chunks: None,
            error_message: None,
            response_status_code: Some(200),
            status: "processing".into(),
            stream: false,
            metrics_latency_ms: None,
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            request_headers: Some(json!({"x-api-key": "***"})),
            request_url: Some("https://api.anthropic.com/v1/messages".into()),
            pass_through_applied: false,
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
        };
        let v = serde_json::to_value(&row)?;
        assert_eq!(v["requestId"], "5");
        assert_eq!(v["responseStatusCode"], 200);
        assert_eq!(v["passThroughApplied"], false);
        // Unset optionals are skipped (mirrors Go omitempty on optional cols).
        assert!(v.as_object().and_then(|o| o.get("responseBody")).is_none());
        assert!(
            v.as_object()
                .and_then(|o| o.get("metricsLatencyMs"))
                .is_none()
        );
        assert!(v.as_object().and_then(|o| o.get("dataStorageId")).is_none());
        let back: RequestExecutionRow = serde_json::from_value(v)?;
        assert_eq!(back, row);
        Ok(())
    }

    /// `OidcIdentityRow` + `ProviderQuotaStatusRow`: soft-delete + optional
    /// timestamp round trip.
    #[test]
    fn oidc_and_provider_quota_rows_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let oidc = OidcIdentityRow {
            id: "1".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "sub-abc".into(),
            email: Some("user@example.com".into()),
            idp_name: None,
            last_login_at: Some(ts("2024-02-01T00:00:00Z")?),
            user_id: "42".into(),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&oidc)?;
        // Unset optional (idp_name) is skipped entirely — absent, not null.
        assert!(v.as_object().and_then(|o| o.get("idpName")).is_none());
        assert_eq!(v["lastLoginAt"], "2024-02-01T00:00:00Z");
        let back: OidcIdentityRow = serde_json::from_value(v)?;
        assert_eq!(back, oidc);

        let quota = ProviderQuotaStatusRow {
            id: "1".into(),
            channel_id: "42".into(),
            provider_type: "codex".into(),
            status: "available".into(),
            quota_data: json!({"remaining": 100}),
            next_reset_at: None,
            ready: true,
            next_check_at: ts("2024-02-01T00:00:00Z")?,
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&quota)?;
        assert_eq!(v["channelId"], "42");
        assert_eq!(v["providerType"], "codex");
        assert_eq!(v["quotaData"]["remaining"], 100);
        assert_eq!(v["nextCheckAt"], "2024-02-01T00:00:00Z");
        let back: ProviderQuotaStatusRow = serde_json::from_value(v)?;
        assert_eq!(back, quota);
        Ok(())
    }

    // --- RUST-P3-002 S13 batch 3 stretch rows --------------------------------

    /// `ChannelProbeRow` has no mixin: neither `created_at`/`updated_at` nor
    /// `deleted_at`; the sample instant is the unix `timestamp` column.
    #[test]
    fn channel_probe_row_has_no_mixin_columns() -> Result<(), Box<dyn std::error::Error>> {
        let probe = ChannelProbeRow {
            id: "1".into(),
            channel_id: "42".into(),
            total_request_count: 10,
            success_request_count: 9,
            avg_tokens_per_second: Some(55.5),
            avg_time_to_first_token_ms: None,
            timestamp: 1_700_000_000,
        };
        let v = serde_json::to_value(&probe)?;
        assert_eq!(v["channelId"], "42");
        assert_eq!(v["totalRequestCount"], 10);
        assert_eq!(v["avgTokensPerSecond"], 55.5);
        assert!(
            v.as_object()
                .and_then(|o| o.get("avgTimeToFirstTokenMs"))
                .is_none()
        );
        assert!(v.as_object().and_then(|o| o.get("createdAt")).is_none());
        assert!(v.as_object().and_then(|o| o.get("deletedAt")).is_none());
        let back: ChannelProbeRow = serde_json::from_value(v)?;
        assert_eq!(back, probe);
        Ok(())
    }

    /// Price rows: head (`ChannelModelPriceRow`, soft-deletable) and version
    /// (`ChannelModelPriceVersionRow`, TimeMixin only, nullable effective end).
    #[test]
    fn channel_model_price_rows_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let head = ChannelModelPriceRow {
            id: "1".into(),
            channel_id: "42".into(),
            model_id: "gpt-4".into(),
            currency_code: "CNY".into(),
            price: json!({"input": 1.5, "output": 2.0}),
            reference_id: "ref-abc".into(),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&head)?;
        assert_eq!(v["referenceId"], "ref-abc");
        assert_eq!(v["currencyCode"], "CNY");
        assert_eq!(v["price"]["input"], 1.5);
        let back: ChannelModelPriceRow = serde_json::from_value(v)?;
        assert_eq!(back, head);

        let version = ChannelModelPriceVersionRow {
            id: "1".into(),
            channel_id: "42".into(),
            model_id: "gpt-4".into(),
            channel_model_price_id: "1".into(),
            currency_code: "CNY".into(),
            price: json!({"input": 1.5}),
            status: "active".into(),
            effective_start_at: ts("2024-01-01T00:00:00Z")?,
            effective_end_at: None,
            reference_id: "ref-abc".into(),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
        };
        let v = serde_json::to_value(&version)?;
        assert_eq!(v["channelModelPriceId"], "1");
        assert_eq!(v["currencyCode"], "CNY");
        assert_eq!(v["effectiveStartAt"], "2024-01-01T00:00:00Z");
        // Open-ended version: effective_end_at absent (Go Nillable).
        assert!(
            v.as_object()
                .and_then(|o| o.get("effectiveEndAt"))
                .is_none()
        );
        assert!(v.as_object().and_then(|o| o.get("deletedAt")).is_none()); // TimeMixin only
        let back: ChannelModelPriceVersionRow = serde_json::from_value(v)?;
        assert_eq!(back, version);
        Ok(())
    }

    /// `ChannelOverrideTemplateRow`: deprecated columns keep their Go
    /// defaults; `PromptProtectionRuleRow` and `ApiKeyProfileTemplateRow`
    /// carry their JSON documents.
    #[test]
    fn template_and_rule_rows_defaults_and_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // Defaults apply when the deprecated/optional JSON columns are absent.
        let tpl: ChannelOverrideTemplateRow = serde_json::from_value(json!({
            "id": "1",
            "name": "my-template",
            "createdAt": "2024-01-01T00:00:00Z",
            "updatedAt": "2024-01-01T00:00:00Z"
        }))?;
        assert_eq!(tpl.user_id, None);
        assert_eq!(tpl.override_parameters, "{}");
        assert_eq!(tpl.override_headers, json!([]));
        assert_eq!(tpl.header_override_operations, json!([]));
        assert_eq!(tpl.body_override_operations, json!([]));
        let back: ChannelOverrideTemplateRow = serde_json::from_value(serde_json::to_value(&tpl)?)?;
        assert_eq!(back, tpl);

        let rule = PromptProtectionRuleRow {
            id: "1".into(),
            name: "no-secrets".into(),
            description: String::new(),
            pattern: "(?i)api[_-]?key".into(),
            status: "disabled".into(),
            settings: json!({"action": "block"}),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&rule)?;
        assert_eq!(v["pattern"], "(?i)api[_-]?key");
        assert_eq!(v["settings"]["action"], "block");
        // Global rule: no project_id column exists on the Go schema.
        assert!(v.as_object().and_then(|o| o.get("projectId")).is_none());
        let back: PromptProtectionRuleRow = serde_json::from_value(v)?;
        assert_eq!(back, rule);

        let tmpl = ApiKeyProfileTemplateRow {
            id: "1".into(),
            name: "default".into(),
            description: String::new(),
            project_id: "3".into(),
            profile: Some(json!({"models": ["gpt-4"]})),
            created_at: ts("2024-01-01T00:00:00Z")?,
            updated_at: ts("2024-01-01T00:00:00Z")?,
            deleted_at: None,
        };
        let v = serde_json::to_value(&tmpl)?;
        assert_eq!(v["projectId"], "3");
        assert_eq!(v["profile"]["models"][0], "gpt-4");
        let back: ApiKeyProfileTemplateRow = serde_json::from_value(v)?;
        assert_eq!(back, tmpl);
        Ok(())
    }
}

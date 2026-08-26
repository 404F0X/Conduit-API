//! Service traits bridging the OpenAPI resolvers to the biz layer.
//!
//! Mirrors the three Go services the OpenAPI resolver depends on
//! (`internal/server/gql/openapi/resolver.go:14-18`):
//!
//! * `biz.APIKeyService` → [`OpenApiApiKeyService`]
//!   (`CreateLLMAPIKey` / `GetForRead` / `UpdateAPIKeyProfiles`);
//! * `biz.APIKeyProfileTemplateService` → [`OpenApiApiKeyProfileTemplateService`]
//!   (`GetForRead` / `LoadTemplate`);
//! * `biz.QuotaService` → [`OpenApiQuotaService`] (`ProfileQuotaUsages`).
//!
//! Go passes authorization implicitly through `context.Context` (the API-key
//! principal injected by `WithOpenAPIAuth`, evaluated by the ent privacy
//! rules). The Rust traits make that explicit: every method takes the caller
//! [`Principal`], and implementations MUST enforce the same contract the Go
//! privacy layer does (A02):
//!
//! * reads require the `read_api_keys` scope and are filtered to the caller's
//!   own project (`scopes.APIKeyProjectScopeReadRule`) — foreign identifiers
//!   surface as a uniform NotFound, never leaking existence;
//! * writes require the `write_api_keys` scope and are project-bounded
//!   (`scopes.APIKeyProjectScopeWriteRule`);
//! * `GetForRead` enforces exactly-one-of on its identifier set
//!   (`biz/api_key.go:707-710`, `biz/api_key_profile_template.go:79-82`).
//!
//! The in-memory implementations used by this crate's tests live in
//! `crate::memory` (test-only); production implementations land with the
//! services crate wiring.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_auth::Principal;
use conduit_core::ConduitError;
use rust_decimal::Decimal;

use crate::model::{APIKeyProfile, APIKeyProfiles, APIKeyQuota};

/// The slice of Go `ent.APIKey` the OpenAPI surface consumes
/// (`toOpenAPIAPIKey`, `openapi/helper.go:57-69`, plus the `ProjectID` the
/// privacy layer scopes by). `id` mirrors Go `int`; `scopes`/`profiles` keep
/// Go's nil-vs-empty distinction because the SDL types them nullable.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyRecord {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub profiles: Option<APIKeyProfiles>,
    /// Owning project — used by implementations for A02 scoping; never
    /// projected onto the GraphQL surface.
    pub project_id: String,
}

/// The slice of Go `ent.APIKeyProfileTemplate` the resolvers consume.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiKeyProfileTemplateRecord {
    pub id: i64,
    pub name: String,
    /// Owning project — templates are project-scoped like keys.
    pub project_id: String,
    /// The template's single profile (Go `*objects.APIKeyProfile`).
    pub profile: Option<APIKeyProfile>,
}

/// Mirrors Go `biz.QuotaWindow` minus `EndInclusive`, which never crosses the
/// GraphQL boundary (`openapi.resolvers.go:111-114` only maps Start/End); the
/// full window math ports with the quota service itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuotaWindow {
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
}

/// Mirrors Go `biz.QuotaUsage` (`quota.go:24-28`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct QuotaUsage {
    pub request_count: i64,
    pub total_tokens: i64,
    pub total_cost: Decimal,
}

/// Mirrors Go `biz.ProfileQuotaUsage` (`quota.go:115-121`) — the per-profile
/// quota usage shared by the admin and OpenAPI resolvers. `quota` is non-nil by
/// construction (entries are only emitted for profiles carrying a quota).
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileQuotaUsage {
    pub profile_name: String,
    pub quota: APIKeyQuota,
    pub window: QuotaWindow,
    pub usage: QuotaUsage,
}

/// `biz.APIKeyService` surface used by the OpenAPI resolvers.
#[async_trait]
pub trait OpenApiApiKeyService: Send + Sync {
    /// Mirrors `CreateLLMAPIKey` (`biz/api_key.go:228-306`): trims the name
    /// (empty → "api key name is required"), creates a `user`-type key in the
    /// caller's project with the fixed scope set
    /// `[read_channels, write_requests]`, and rejects duplicate live names
    /// within the project ("API Key name '<name>' already exists"). Requires
    /// the `write_api_keys` scope (privacy mutation policy).
    async fn create_llm_api_key(
        &self,
        caller: &Principal,
        name: &str,
    ) -> Result<ApiKeyRecord, ConduitError>;

    /// Mirrors `GetForRead` (`biz/api_key.go:707-740`): exactly one of
    /// `id`/`key`/`name` must be provided ("exactly one of api key id, key, or
    /// name must be provided"); requires `read_api_keys`; only keys inside the
    /// caller's project are visible (uniform NotFound otherwise).
    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        key: Option<&str>,
        name: Option<&str>,
    ) -> Result<ApiKeyRecord, ConduitError>;

    /// Mirrors `UpdateAPIKeyProfiles`: replaces the key's profile set.
    /// Requires `write_api_keys` and the key to live in the caller's project.
    async fn update_api_key_profiles(
        &self,
        caller: &Principal,
        id: i64,
        profiles: APIKeyProfiles,
    ) -> Result<ApiKeyRecord, ConduitError>;
}

/// `biz.APIKeyProfileTemplateService` surface used by the OpenAPI resolvers.
#[async_trait]
pub trait OpenApiApiKeyProfileTemplateService: Send + Sync {
    /// Mirrors `GetForRead` (`biz/api_key_profile_template.go:78-100`):
    /// exactly one of `id`/`name` ("exactly one of template id or name must be
    /// provided"); `read_api_keys` + own-project filtering.
    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        name: Option<&str>,
    ) -> Result<ApiKeyProfileTemplateRecord, ConduitError>;

    /// Mirrors `LoadTemplate` (`biz/api_key_profile_template.go:181-233`):
    /// append-only — clones the template's profile, resolves name conflicts by
    /// suffixing " (n)", appends it to the key's profiles and leaves
    /// `activeProfile` untouched. Template and key must belong to the same
    /// project; a template without a profile is an error.
    async fn load_template(
        &self,
        caller: &Principal,
        template_id: i64,
        api_key_id: i64,
    ) -> Result<ApiKeyRecord, ConduitError>;
}

/// `biz.QuotaService` surface used by the OpenAPI resolvers.
#[async_trait]
pub trait OpenApiQuotaService: Send + Sync {
    /// Mirrors `ProfileQuotaUsages` (`biz/quota.go:125-155`): for every
    /// profile on the key that has a quota configured, returns the realtime
    /// usage in that quota's window. The caller is responsible for loading the
    /// key (and thereby applying authorization) — Go runs the aggregate reads
    /// under a system bypass, so no extra scope is required here.
    async fn profile_quota_usages(
        &self,
        caller: &Principal,
        api_key: &ApiKeyRecord,
    ) -> Result<Vec<ProfileQuotaUsage>, ConduitError>;
}

/// Dependency bundle injected into the schema, mirroring Go's `Resolver`
/// struct (`openapi/resolver.go:14-18`). Stored as `Schema::data` so every
/// resolver can reach it.
#[derive(Clone)]
pub struct OpenApiServices {
    pub api_keys: Arc<dyn OpenApiApiKeyService>,
    pub templates: Arc<dyn OpenApiApiKeyProfileTemplateService>,
    pub quota: Arc<dyn OpenApiQuotaService>,
}

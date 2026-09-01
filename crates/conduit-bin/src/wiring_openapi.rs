//! WIRE-OPENAPI — DB-backed production implementations of the three OpenAPI
//! GraphQL service seams the resolver layer depends on
//! (`conduit_openapi_graphql::service`), plus the [`build_openapi_services`]
//! constructor the Leader injects into [`conduit_openapi_graphql::build_openapi_schema`].
//!
//! The `conduit-openapi-graphql` crate ships only the *test-only* in-memory
//! implementations (`crate::memory::InMemoryOpenApi`, `#[cfg(test)]`). Without a
//! production bundle the HTTP layer's `AppState.openapi_schema()` is `None` and
//! `POST /openapi/v1/graphql` returns 503. This file supplies the three
//! adapters + the pool-based constructor that closes that gap.
//!
//! ## Adapters
//!
//! * [`OpenApiApiKeyAdapter`] → [`OpenApiApiKeyService`]
//!   (`create_llm_api_key` / `get_for_read` / `update_api_key_profiles`) over
//!   the backend-neutral [`ApiKeyRepo`] seam.
//! * [`OpenApiProfileTemplateAdapter`] →
//!   [`OpenApiApiKeyProfileTemplateService`] (`get_for_read` / `load_template`)
//!   over [`ProfileTemplateRepo`] + [`ApiKeyRepo`].
//! * [`OpenApiQuotaAdapter`] → [`OpenApiQuotaService`] (`profile_quota_usages`)
//!   over [`UsageRepo`] via [`QuotaService::profile_quota_usages`].
//!
//! ## Go parity anchors (behaviour mirrored EXACTLY from the memory reference +
//! the Go biz layer the doc comments on the traits cite)
//!
//! * `CreateLLMAPIKey` (`biz/api_key.go:228-306`): trim name (empty →
//!   `"api key name is required"`), fixed scope set
//!   `[read_channels, write_requests]`, `type = user`, per-project live-name
//!   uniqueness (`"API Key name '<name>' already exists"`). Requires
//!   `write_api_keys`.
//! * `GetForRead` (`biz/api_key.go:707-738`): exactly-one-of id/key/name
//!   (`"exactly one of api key id, key, or name must be provided"`), requires
//!   `read_api_keys`, own-project filter → uniform NotFound.
//! * `UpdateAPIKeyProfiles`: replace the profile set, `write_api_keys`,
//!   own-project (the OpenAPI resolver already normalizes nil `modelMappings`
//!   before this call — that lives in the model layer, not here).
//! * template `GetForRead` (`biz/api_key_profile_template.go:78-99`):
//!   exactly-one-of id/name (`"exactly one of template id or name must be
//!   provided"`), `read_api_keys`, own-project.
//! * `LoadTemplate` (`biz/api_key_profile_template.go:181-233`): append-only —
//!   clone the template profile, resolve the name conflict by suffixing
//!   `" (n)"`, append to the key's profiles, leave `activeProfile` untouched;
//!   `write_api_keys`; template + key must share the caller's project; a
//!   NULL-profile template is `"template has no profile"`.
//! * `ProfileQuotaUsages` (`biz/quota.go:125-155`): for every profile carrying a
//!   quota, emit the realtime usage over that quota's window; profiles without a
//!   quota are skipped; the aggregate reads run under a system bypass so no extra
//!   scope is required.
//!
//! ## Authorization model
//!
//! The trait `caller: &conduit_auth::Principal` is the service-account principal
//! the HTTP layer injects (`WithOpenAPIAuth`). Each method enforces the scope +
//! own-project contract against that principal EXPLICITLY (mirroring
//! `crate::memory`), then talks to the repos through a trusted
//! [`conduit_db::Principal::test`] context (the same boot-context convention as
//! every sibling `wiring_*` adapter) — so the DB-layer project filter never
//! double-denies a request this layer already authorized. Project scoping on
//! reads is applied in-adapter (the repos' `find_by_id` / `find_by_key` are not
//! project-filtered), exactly as the in-memory reference does.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, FixedOffset, Utc};
use conduit_auth::Principal;
use conduit_auth::apikey::generate_api_key;
use conduit_auth::scopes::slug;
use conduit_core::objects::apikey::{
    APIKeyProfile as CoreProfile, APIKeyProfiles as CoreProfiles, APIKeyQuota as CoreQuota,
    APIKeyQuotaCalendarDuration as CoreCalDuration, APIKeyQuotaPastDuration as CorePastDuration,
    APIKeyQuotaPeriod as CoreQuotaPeriod, ModelMapping as CoreModelMapping,
};
use conduit_core::{ConduitError, ErrorKind};
use conduit_db::repo::ApiKeyRepo;
use conduit_db::repo::api_key_repo::{
    CreateApiKeyInput as RepoCreateApiKeyInput, ListApiKeysQuery,
    UpdateApiKeyInput as RepoUpdateApiKeyInput,
};
use conduit_db::repo::profile_template_repo::ProfileTemplateRepo;
use conduit_db::row::ApiKeyRow;
use conduit_db::{
    PolicyContext, Principal as DbPrincipal, RepoError, RequestContext, UsageAggregateQuery,
    UsageRepo,
};
use conduit_openapi_graphql::{
    APIKeyProfile as GqlProfile, APIKeyProfiles as GqlProfiles, APIKeyQuota as GqlQuota,
    APIKeyQuotaCalendarDuration as GqlCalDuration, APIKeyQuotaCalendarDurationUnit as GqlCalUnit,
    APIKeyQuotaPastDuration as GqlPastDuration, APIKeyQuotaPastDurationUnit as GqlPastUnit,
    APIKeyQuotaPeriod as GqlPeriod, APIKeyQuotaPeriodType as GqlPeriodType,
    ApiKeyProfileTemplateRecord, ApiKeyRecord, ChannelTagsMatchMode as GqlTagsMode, GqlDecimal,
    ModelMapping as GqlModelMapping, OpenApiApiKeyProfileTemplateService, OpenApiApiKeyService,
    OpenApiQuotaService, OpenApiServices, ProfileQuotaUsage as GqlProfileQuotaUsage,
    QuotaUsage as GqlQuotaUsage, QuotaWindow as GqlQuotaWindow,
};
use conduit_services::apikey_service::validate_all_profiles;
use conduit_services::{
    QuotaService, QuotaUsageAggregate, QuotaUsageRepo, QuotaWindow as ServiceQuotaWindow,
};
use rust_decimal::Decimal;

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Trusted boot context for repo calls. Mirrors `wiring::boot_request_context`
/// and every sibling adapter: the OpenAPI HTTP layer performs auth before the
/// resolver runs, and this file re-checks scope + project against the caller
/// principal, so the repo access itself uses the `Test` bypass principal.
fn repo_ctx() -> RequestContext {
    RequestContext::new(PolicyContext::new(DbPrincipal::test()))
}

/// Scope check mirroring the ent privacy `Denyf` text
/// (`internal/scopes/rule_apikey_scope.go:33`): "API key does not have required
/// scope: <scope>".
fn require_scope(caller: &Principal, scope: &str) -> Result<(), ConduitError> {
    if caller.scopes.contains(scope) {
        return Ok(());
    }
    Err(ConduitError::new(
        ErrorKind::Forbidden,
        format!("API key does not have required scope: {scope}"),
    ))
}

/// The caller's project — `WithOpenAPIAuth` always injects it for service
/// accounts; a missing project is a wiring bug we refuse to widen.
fn caller_project(caller: &Principal) -> Result<String, ConduitError> {
    caller
        .project_id
        .clone()
        .ok_or_else(|| ConduitError::new(ErrorKind::Forbidden, "principal lacks a project_id"))
}

/// A repo failure that is not one of the semantically-mapped cases becomes an
/// internal error carrying the backend message.
fn repo_error_to_axon(err: RepoError) -> ConduitError {
    ConduitError::new(ErrorKind::Internal, err.to_string())
}

/// Uniform NotFound for a missing/foreign api key (Go surfaces ent's "not
/// found"; foreign or missing keys are indistinguishable — no existence leak).
fn api_key_not_found() -> ConduitError {
    ConduitError::new(ErrorKind::NotFound, "api_key not found")
}

/// Uniform NotFound for a missing/foreign template.
fn template_not_found() -> ConduitError {
    ConduitError::new(ErrorKind::NotFound, "api_key_profile_template not found")
}

/// Convert integer-micros (accounting unit × 1_000_000, the `UsageAggregate`
/// cost domain) into a scale-6 [`Decimal`]. Mirrors the `usage_repo` convention.
fn micros_to_decimal(micros: i64) -> Decimal {
    Decimal::new(micros, 6)
}

/// UTC [`FixedOffset`] for `calendar_duration` bucket alignment (Go's `time.UTC`
/// fallback when `SystemService.TimeLocation` resolves to UTC). Only
/// `calendar_duration` windows depend on it; `all_time` / `past_duration` are
/// timezone-independent.
fn utc_offset() -> Result<FixedOffset, ConduitError> {
    FixedOffset::east_opt(0)
        .ok_or_else(|| ConduitError::new(ErrorKind::Internal, "failed to build UTC offset"))
}

// Verbatim port of Go `resolveProfileNameConflict`
// (`biz/api_key_profile_template.go:235-251`): first free "<name>" or
// "<name> (i)" starting at i=1.
fn resolve_profile_name_conflict(existing: &[CoreProfile], new_name: &str) -> String {
    let taken: BTreeSet<&str> = existing.iter().map(|p| p.name.as_str()).collect();
    if !taken.contains(new_name) {
        return new_name.to_string();
    }
    let mut i: i64 = 1;
    loop {
        let candidate = format!("{new_name} ({i})");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        i += 1;
    }
}

// ---------------------------------------------------------------------------
// Enum <-> wire-string maps (wire strings are the Go/GraphQL bound values,
// identical to the private maps in wiring_apikey.rs).
// ---------------------------------------------------------------------------

fn gql_period_type_to_wire(t: GqlPeriodType) -> &'static str {
    match t {
        GqlPeriodType::AllTime => "all_time",
        GqlPeriodType::PastDuration => "past_duration",
        GqlPeriodType::CalendarDuration => "calendar_duration",
    }
}

fn wire_to_gql_period_type(s: &str) -> GqlPeriodType {
    match s {
        "past_duration" => GqlPeriodType::PastDuration,
        "calendar_duration" => GqlPeriodType::CalendarDuration,
        _ => GqlPeriodType::AllTime,
    }
}

fn gql_past_unit_to_wire(u: GqlPastUnit) -> &'static str {
    match u {
        GqlPastUnit::Minute => "minute",
        GqlPastUnit::Hour => "hour",
        GqlPastUnit::Day => "day",
    }
}

fn wire_to_gql_past_unit(s: &str) -> GqlPastUnit {
    match s {
        "hour" => GqlPastUnit::Hour,
        "day" => GqlPastUnit::Day,
        _ => GqlPastUnit::Minute,
    }
}

fn gql_cal_unit_to_wire(u: GqlCalUnit) -> &'static str {
    match u {
        GqlCalUnit::Day => "day",
        GqlCalUnit::Month => "month",
    }
}

fn wire_to_gql_cal_unit(s: &str) -> GqlCalUnit {
    match s {
        "month" => GqlCalUnit::Month,
        _ => GqlCalUnit::Day,
    }
}

fn gql_tags_mode_to_wire(m: GqlTagsMode) -> &'static str {
    match m {
        GqlTagsMode::Any => "any",
        GqlTagsMode::All => "all",
        GqlTagsMode::None => "none",
    }
}

fn wire_to_gql_tags_mode(s: &str) -> Option<GqlTagsMode> {
    match s {
        "any" => Some(GqlTagsMode::Any),
        "all" => Some(GqlTagsMode::All),
        "none" => Some(GqlTagsMode::None),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// core <-> GraphQL profile/quota conversions. The core objects' serde tags
// produce the exact Go on-disk JSON (`objects.APIKeyProfile` camelCase +
// acronym renames); the GraphQL model types are what the resolvers project.
// ---------------------------------------------------------------------------

fn core_quota_to_gql(q: CoreQuota) -> GqlQuota {
    GqlQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(GqlDecimal),
        period: GqlPeriod {
            r#type: wire_to_gql_period_type(&q.period.r#type),
            past_duration: q.period.past_duration.map(|pd| GqlPastDuration {
                value: pd.value,
                unit: wire_to_gql_past_unit(&pd.unit),
            }),
            calendar_duration: q.period.calendar_duration.map(|cd| GqlCalDuration {
                unit: wire_to_gql_cal_unit(&cd.unit),
            }),
        },
    }
}

fn gql_quota_to_core(q: &GqlQuota) -> CoreQuota {
    CoreQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(|c| c.0),
        period: CoreQuotaPeriod {
            r#type: gql_period_type_to_wire(q.period.r#type).to_string(),
            past_duration: q.period.past_duration.map(|pd| CorePastDuration {
                value: pd.value,
                unit: gql_past_unit_to_wire(pd.unit).to_string(),
            }),
            calendar_duration: q.period.calendar_duration.map(|cd| CoreCalDuration {
                unit: gql_cal_unit_to_wire(cd.unit).to_string(),
            }),
        },
    }
}

fn core_profile_to_gql(p: CoreProfile) -> GqlProfile {
    GqlProfile {
        name: p.name,
        model_mappings: if p.model_mappings.is_empty() {
            None
        } else {
            Some(
                p.model_mappings
                    .into_iter()
                    .map(|m| GqlModelMapping {
                        from: m.from,
                        to: m.to,
                    })
                    .collect(),
            )
        },
        channel_ids: if p.channel_ids.is_empty() {
            None
        } else {
            Some(p.channel_ids)
        },
        channel_tags: if p.channel_tags.is_empty() {
            None
        } else {
            Some(p.channel_tags)
        },
        channel_tags_match_mode: p
            .channel_tags_match_mode
            .as_deref()
            .and_then(wire_to_gql_tags_mode),
        model_ids: if p.model_ids.is_empty() {
            None
        } else {
            Some(p.model_ids)
        },
        valid_from: p.valid_from.map(conduit_openapi_graphql::scalars::GqlTime),
        valid_until: p.valid_until.map(conduit_openapi_graphql::scalars::GqlTime),
        quota: p.quota.map(core_quota_to_gql),
        load_balance_strategy: p.load_balance_strategy,
    }
}

fn gql_profile_to_core(p: GqlProfile) -> CoreProfile {
    CoreProfile {
        name: p.name,
        model_mappings: p
            .model_mappings
            .unwrap_or_default()
            .into_iter()
            .map(|m| CoreModelMapping {
                from: m.from,
                to: m.to,
            })
            .collect(),
        quota: p.quota.as_ref().map(gql_quota_to_core),
        load_balance_strategy: p.load_balance_strategy,
        max_concurrent_requests: None,
        channel_ids: p.channel_ids.unwrap_or_default(),
        channel_tags: p.channel_tags.unwrap_or_default(),
        channel_tags_match_mode: p
            .channel_tags_match_mode
            .map(|m| gql_tags_mode_to_wire(m).to_string()),
        model_ids: p.model_ids.unwrap_or_default(),
        valid_from: p.valid_from.map(|value| value.0),
        valid_until: p.valid_until.map(|value| value.0),
    }
}

fn core_profiles_to_gql(core: CoreProfiles) -> GqlProfiles {
    let profiles: Vec<GqlProfile> = core.profiles.into_iter().map(core_profile_to_gql).collect();
    GqlProfiles {
        active_profile: core.active_profile,
        profiles: if profiles.is_empty() {
            None
        } else {
            Some(profiles)
        },
    }
}

fn gql_profiles_to_core(p: GqlProfiles) -> CoreProfiles {
    CoreProfiles {
        active_profile: p.active_profile,
        profiles: p
            .profiles
            .unwrap_or_default()
            .into_iter()
            .map(gql_profile_to_core)
            .collect(),
    }
}

/// Project the api-key `profiles` JSON column onto the OpenAPI surface. Mirrors
/// Go `toOpenAPIAPIKey`, which passes `k.Profiles` straight through: a NULL
/// column (nil pointer) → GraphQL null; a `{}` value → a non-null (empty)
/// object. A malformed blob degrades to the empty struct rather than failing
/// the whole read (a single bad row must not black out the surface).
fn row_profiles_to_gql(value: &serde_json::Value) -> Option<GqlProfiles> {
    if value.is_null() {
        return None;
    }
    let core: CoreProfiles = serde_json::from_value(value.clone()).unwrap_or_default();
    Some(core_profiles_to_gql(core))
}

/// Convert a live [`ApiKeyRow`] into the OpenAPI [`ApiKeyRecord`]. The full key
/// is returned (no masking — Go `toOpenAPIAPIKey` returns the raw `Key`).
fn row_to_record(row: ApiKeyRow) -> Result<ApiKeyRecord, ConduitError> {
    let id = row
        .id
        .parse::<i64>()
        .map_err(|_| ConduitError::new(ErrorKind::Internal, "api key id is not a valid integer"))?;
    Ok(ApiKeyRecord {
        id,
        key: row.key,
        name: row.name,
        // Go `toOpenAPIAPIKey` keeps `k.Scopes` (the ent []string, never nil).
        scopes: Some(row.scopes),
        profiles: row_profiles_to_gql(&row.profiles),
        project_id: row.project_id,
    })
}

// ===========================================================================
// Live usage-repo bridge (backs `QuotaService::profile_quota_usages`)
// ===========================================================================

/// Bridges the domain [`QuotaUsageRepo`] surface (the two queries the quota
/// dashboard path issues) onto a live backend [`UsageRepo`]. Mirrors Go's
/// `RunWithSystemBypass` usage queries: it uses `aggregate_usage_unchecked` so
/// the read is not blocked by per-project authz (the caller already authorized
/// the parent API key by loading it).
///
/// `project_id` scopes the aggregate query. An API key belongs to exactly one
/// project and all its usage rows carry that `project_id`, so filtering by
/// `(project_id, api_key_id)` yields the same rows as filtering by `api_key_id`
/// alone — while satisfying `UsageAggregateQuery`'s required project scope.
struct OpenApiUsageBridge {
    usage_repo: Arc<dyn UsageRepo>,
    ctx: RequestContext,
    project_id: String,
}

impl OpenApiUsageBridge {
    /// Lower a [`ServiceQuotaWindow`] + `api_key_id` into a `UsageAggregateQuery`.
    /// The aggregate query's date range is inclusive on both bounds; Go's
    /// `all_time` / `past_duration` windows are inclusive-end (mapped directly),
    /// while a `calendar_duration` window is exclusive-end, so its upper bound is
    /// nudged back one microsecond to exclude a row timestamped exactly at the
    /// bucket boundary (Go
    /// `TestQuotaService_CalendarDuration_ExcludesUsageAtWindowEnd`).
    fn lower_query(&self, api_key_id: &str, window: &ServiceQuotaWindow) -> UsageAggregateQuery {
        let start_at = window.start.map(|dt| dt.to_rfc3339());
        let end_at = window.end.map(|dt| {
            let bound = if window.end_inclusive {
                dt
            } else {
                dt - Duration::microseconds(1)
            };
            bound.to_rfc3339()
        });
        UsageAggregateQuery {
            project_id: self.project_id.clone(),
            start_at,
            end_at,
            api_key_id: Some(api_key_id.to_string()),
            channel_id: None,
            model_id: None,
            source: None,
        }
    }
}

#[async_trait]
impl QuotaUsageRepo for OpenApiUsageBridge {
    async fn request_count(
        &self,
        api_key_id: &str,
        window: &ServiceQuotaWindow,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let query = self.lower_query(api_key_id, window);
        let agg = self
            .usage_repo
            .aggregate_usage_unchecked(&self.ctx, query)
            .await
            .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))?;
        Ok(agg.request_count)
    }

    async fn usage_aggregate(
        &self,
        api_key_id: &str,
        window: &ServiceQuotaWindow,
    ) -> Result<QuotaUsageAggregate, Box<dyn std::error::Error + Send + Sync>> {
        let query = self.lower_query(api_key_id, window);
        let agg = self
            .usage_repo
            .aggregate_usage_unchecked(&self.ctx, query)
            .await
            .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(e.to_string()))?;
        Ok(QuotaUsageAggregate {
            requests: agg.request_count,
            tokens: agg.total_tokens,
            cost: micros_to_decimal(agg.total_cost_micros),
        })
    }
}

// ===========================================================================
// OpenApiApiKeyAdapter — biz.APIKeyService surface
// ===========================================================================

/// DB-backed [`OpenApiApiKeyService`] over the backend-neutral [`ApiKeyRepo`].
pub struct OpenApiApiKeyAdapter {
    api_key_repo: Arc<dyn ApiKeyRepo>,
    /// Configured api-key prefix (`server.api.auth.key_prefix`, Go default `ah`).
    key_prefix: String,
}

impl OpenApiApiKeyAdapter {
    pub fn new(api_key_repo: Arc<dyn ApiKeyRepo>, key_prefix: String) -> Self {
        Self {
            api_key_repo,
            key_prefix,
        }
    }

    /// Find one LIVE key by name inside `project`. Names are unique per project
    /// (enforced on create/update), so the first match identifies the key. The
    /// repo has no by-name finder, so the project's keys are paged and scanned —
    /// same convention as `wiring_apikey::load_all_live`.
    async fn find_key_by_name_in_project(
        &self,
        ctx: &RequestContext,
        project: &str,
        name: &str,
    ) -> Result<Option<ApiKeyRow>, ConduitError> {
        const PAGE: u32 = 500;
        let mut offset = 0u32;
        loop {
            let query = ListApiKeysQuery {
                limit: PAGE,
                offset,
                project_id: Some(project.to_string()),
                ..Default::default()
            };
            let result = self
                .api_key_repo
                .list_api_keys(ctx, &query)
                .await
                .map_err(repo_error_to_axon)?;
            if let Some(found) = result
                .rows
                .iter()
                .find(|r| r.name == name && r.project_id == project)
            {
                return Ok(Some(found.clone()));
            }
            let fetched = result.rows.len();
            if !result.has_more || fetched == 0 {
                return Ok(None);
            }
            offset = offset.saturating_add(PAGE);
        }
    }
}

#[async_trait]
impl OpenApiApiKeyService for OpenApiApiKeyAdapter {
    async fn create_llm_api_key(
        &self,
        caller: &Principal,
        name: &str,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Go order (`biz/api_key.go:229-232`): trim + required-name check runs
        // before the privacy mutation policy (which fires at Save).
        let name = name.trim();
        if name.is_empty() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "api key name is required",
            ));
        }

        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let generated = generate_api_key(&self.key_prefix).map_err(|e| {
            ConduitError::new(
                ErrorKind::Internal,
                format!("failed to generate api key: {e}"),
            )
        })?;

        let ctx = repo_ctx();
        let now = Utc::now().to_rfc3339();
        let input = RepoCreateApiKeyInput {
            id: String::new(), // PostgreSQL owns the generated PK
            // Go stamps `owner.UserID`; the OpenAPI principal does not carry the
            // owning user id (only the api-key id + project), so the row is
            // created with `user_id = NULL` — same documented divergence as
            // `wiring_apikey::create_api_key`.
            user_id: None,
            project_id: project,
            name: name.to_string(),
            key: generated,
            key_type: "user".to_string(),
            // Fixed scope set from `biz/api_key.go:261-264`.
            scopes: vec![
                slug::READ_CHANNELS.to_string(),
                slug::WRITE_REQUESTS.to_string(),
            ],
            profiles: None,
            created_at: now,
        };

        let row = self
            .api_key_repo
            .create_api_key(&ctx, input)
            .await
            .map_err(|e| match e {
                // Go `xerrors.DuplicateNameError("API Key", name)`.
                RepoError::NameConflict => ConduitError::new(
                    ErrorKind::Conflict,
                    format!("API Key name '{name}' already exists"),
                ),
                other => ConduitError::new(
                    ErrorKind::Internal,
                    format!("failed to create api key: {other}"),
                ),
            })?;
        row_to_record(row)
    }

    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        key: Option<&str>,
        name: Option<&str>,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Exactly-one-of first, mirroring `biz/api_key.go:708-710`.
        let provided =
            usize::from(id.is_some()) + usize::from(key.is_some()) + usize::from(name.is_some());
        if provided != 1 {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "exactly one of api key id, key, or name must be provided",
            ));
        }

        // Privacy read rule: read_api_keys + own-project filter.
        require_scope(caller, slug::READ_API_KEYS)?;
        let project = caller_project(caller)?;

        let ctx = repo_ctx();
        let row = if let Some(id) = id {
            self.api_key_repo
                .find_api_key_by_id(&ctx, &id.to_string())
                .await
                .map_err(repo_error_to_axon)?
        } else if let Some(key) = key {
            self.api_key_repo
                .find_api_key_by_key(&ctx, key)
                .await
                .map_err(repo_error_to_axon)?
        } else if let Some(name) = name {
            self.find_key_by_name_in_project(&ctx, &project, name)
                .await?
        } else {
            None
        };

        // Own-project filter → uniform NotFound (foreign or missing keys are
        // indistinguishable; no existence leak).
        match row {
            Some(row) if row.project_id == project => row_to_record(row),
            _ => Err(api_key_not_found()),
        }
    }

    async fn update_api_key_profiles(
        &self,
        caller: &Principal,
        id: i64,
        profiles: GqlProfiles,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // Privacy mutation rule: write_api_keys + own-project row filter.
        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let ctx = repo_ctx();
        let existing = self
            .api_key_repo
            .find_api_key_by_id(&ctx, &id.to_string())
            .await
            .map_err(repo_error_to_axon)?;
        match &existing {
            Some(row) if row.project_id == project => {}
            _ => return Err(api_key_not_found()),
        }

        // Lower the GraphQL profiles into the canonical Go on-disk JSON shape by
        // building the core objects and serializing them (guarantees the exact
        // camelCase + omitempty layout Go persists and the read path parses).
        let core = gql_profiles_to_core(profiles);
        validate_all_profiles(&core).map_err(|error| {
            ConduitError::invalid_request(format!("invalid API key profiles: {error}"))
        })?;
        let profiles_json = serde_json::to_value(&core).map_err(|e| {
            ConduitError::new(
                ErrorKind::Internal,
                format!("failed to serialize api key profiles: {e}"),
            )
        })?;

        let now = Utc::now().to_rfc3339();
        let row = self
            .api_key_repo
            .update_api_key(
                &ctx,
                &id.to_string(),
                RepoUpdateApiKeyInput {
                    profiles: Some(profiles_json),
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => api_key_not_found(),
                other => repo_error_to_axon(other),
            })?;
        row_to_record(row)
    }
}

// ===========================================================================
// OpenApiProfileTemplateAdapter — biz.APIKeyProfileTemplateService surface
// ===========================================================================

/// DB-backed [`OpenApiApiKeyProfileTemplateService`] over
/// [`ProfileTemplateRepo`] (template CRUD) + [`ApiKeyRepo`] (the
/// `loadApiKeyProfileTemplate` append-to-key mutation touches both entities).
pub struct OpenApiProfileTemplateAdapter {
    template_repo: Arc<dyn ProfileTemplateRepo>,
    api_key_repo: Arc<dyn ApiKeyRepo>,
}

impl OpenApiProfileTemplateAdapter {
    pub fn new(
        template_repo: Arc<dyn ProfileTemplateRepo>,
        api_key_repo: Arc<dyn ApiKeyRepo>,
    ) -> Self {
        Self {
            template_repo,
            api_key_repo,
        }
    }

    /// Find one LIVE template by name inside `project`. Template names are unique
    /// per `(project_id, name)` (DB index), so the first match identifies it.
    async fn find_template_by_name(
        &self,
        ctx: &RequestContext,
        project: &str,
        name: &str,
    ) -> Result<
        Option<conduit_db::repo::profile_template_repo::ApiKeyProfileTemplateRow>,
        ConduitError,
    > {
        let rows = self
            .template_repo
            .list_profile_templates(ctx, project)
            .await
            .map_err(repo_error_to_axon)?;
        Ok(rows.into_iter().find(|t| t.name == name))
    }
}

#[async_trait]
impl OpenApiApiKeyProfileTemplateService for OpenApiProfileTemplateAdapter {
    async fn get_for_read(
        &self,
        caller: &Principal,
        id: Option<i64>,
        name: Option<&str>,
    ) -> Result<ApiKeyProfileTemplateRecord, ConduitError> {
        // Exactly-one-of (`biz/api_key_profile_template.go:79-82`).
        if id.is_some() == name.is_some() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "exactly one of template id or name must be provided",
            ));
        }

        require_scope(caller, slug::READ_API_KEYS)?;
        let project = caller_project(caller)?;

        let ctx = repo_ctx();
        let row = if let Some(id) = id {
            // `find_profile_template` is keyed by `(project_id, id)`, so a
            // foreign template is invisible → NotFound.
            self.template_repo
                .find_profile_template(&ctx, &project, &id.to_string())
                .await
                .map_err(repo_error_to_axon)?
        } else if let Some(name) = name {
            self.find_template_by_name(&ctx, &project, name).await?
        } else {
            None
        };

        let row = row.ok_or_else(template_not_found)?;
        Ok(ApiKeyProfileTemplateRecord {
            id: row.id.parse::<i64>().map_err(|_| {
                ConduitError::new(ErrorKind::Internal, "template id is not a valid integer")
            })?,
            name: row.name,
            project_id: row.project_id,
            profile: template_row_profile_to_gql(row.profile.as_ref())?,
        })
    }

    async fn load_template(
        &self,
        caller: &Principal,
        template_id: i64,
        api_key_id: i64,
    ) -> Result<ApiKeyRecord, ConduitError> {
        // The Save runs under the write mutation rule; the inner Gets under the
        // read rules with the project filter.
        require_scope(caller, slug::WRITE_API_KEYS)?;
        let project = caller_project(caller)?;

        let ctx = repo_ctx();

        // Load template (project-scoped by the repo) → NotFound otherwise.
        let template = self
            .template_repo
            .find_profile_template(&ctx, &project, &template_id.to_string())
            .await
            .map_err(repo_error_to_axon)?
            .ok_or_else(template_not_found)?;

        // Load key and enforce the own-project filter.
        let key_row = self
            .api_key_repo
            .find_api_key_by_id(&ctx, &api_key_id.to_string())
            .await
            .map_err(repo_error_to_axon)?;
        let key_row = match key_row {
            Some(row) if row.project_id == project => row,
            _ => return Err(api_key_not_found()),
        };

        // Same-project guard verbatim from `LoadTemplate` (go:196-198) — kept
        // even though the project filters above already imply it.
        if template.project_id != key_row.project_id {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "template and API key must belong to the same project",
            ));
        }

        // Clone the template profile (go:200-203). A NULL profile column → error.
        let template_profile: CoreProfile = match &template.profile {
            Some(value) => serde_json::from_value(value.clone()).map_err(|e| {
                ConduitError::new(
                    ErrorKind::Internal,
                    format!("failed to decode template profile: {e}"),
                )
            })?,
            None => {
                return Err(ConduitError::new(
                    ErrorKind::InvalidRequest,
                    "template has no profile",
                ));
            }
        };

        // Nil profiles → empty container (go:205-208); activeProfile untouched.
        let mut existing: CoreProfiles =
            serde_json::from_value(key_row.profiles.clone()).unwrap_or_default();

        // Profile-name fallback + conflict resolution (go:210-215).
        let base_name = if template_profile.name.is_empty() {
            template.name.clone()
        } else {
            template_profile.name.clone()
        };
        let resolved_name = resolve_profile_name_conflict(&existing.profiles, &base_name);
        let mut appended = template_profile;
        appended.name = resolved_name;

        // Append-only (go:217).
        existing.profiles.push(appended);

        let profiles_json = serde_json::to_value(&existing).map_err(|e| {
            ConduitError::new(
                ErrorKind::Internal,
                format!("failed to serialize api key profiles: {e}"),
            )
        })?;

        let now = Utc::now().to_rfc3339();
        let updated = self
            .api_key_repo
            .update_api_key(
                &ctx,
                &api_key_id.to_string(),
                RepoUpdateApiKeyInput {
                    profiles: Some(profiles_json),
                    updated_at: now,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => api_key_not_found(),
                other => repo_error_to_axon(other),
            })?;
        row_to_record(updated)
    }
}

/// Decode a template's `profile` JSON column (Go `*objects.APIKeyProfile`) into
/// the OpenAPI surface profile. A NULL column → `None` (Go nil pointer); a value
/// deserializes through the core object then projects onto the GraphQL type.
fn template_row_profile_to_gql(
    profile: Option<&serde_json::Value>,
) -> Result<Option<GqlProfile>, ConduitError> {
    match profile {
        None => Ok(None),
        Some(value) if value.is_null() => Ok(None),
        Some(value) => {
            let core: CoreProfile = serde_json::from_value(value.clone()).map_err(|e| {
                ConduitError::new(
                    ErrorKind::Internal,
                    format!("failed to decode template profile: {e}"),
                )
            })?;
            Ok(Some(core_profile_to_gql(core)))
        }
    }
}

// ===========================================================================
// OpenApiQuotaAdapter — biz.QuotaService.ProfileQuotaUsages surface
// ===========================================================================

/// DB-backed [`OpenApiQuotaService`] over a backend [`UsageRepo`] via the domain
/// [`QuotaService`]. Non-gating dashboard path: it always queries both
/// aggregates and never rejects (Go `ProfileQuotaUsages` → `GetQuota`).
pub struct OpenApiQuotaAdapter {
    usage_repo: Arc<dyn UsageRepo>,
}

impl OpenApiQuotaAdapter {
    pub fn new(usage_repo: Arc<dyn UsageRepo>) -> Self {
        Self { usage_repo }
    }
}

#[async_trait]
impl OpenApiQuotaService for OpenApiQuotaAdapter {
    async fn profile_quota_usages(
        &self,
        _caller: &Principal,
        api_key: &ApiKeyRecord,
    ) -> Result<Vec<GqlProfileQuotaUsage>, ConduitError> {
        // Go returns nil for a key with no profiles (`quota.go:130-132`); each
        // `(name, quota)` pair is streamed to the quota service, which skips the
        // ones whose quota is nil (`quota.go:137-139`).
        let profile_pairs: Vec<(String, Option<CoreQuota>)> = match &api_key.profiles {
            Some(profiles) => match &profiles.profiles {
                Some(list) => list
                    .iter()
                    .map(|p| (p.name.clone(), p.quota.as_ref().map(gql_quota_to_core)))
                    .collect(),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        if profile_pairs.is_empty() {
            return Ok(Vec::new());
        }

        let offset = utc_offset()?;
        let bridge = OpenApiUsageBridge {
            usage_repo: Arc::clone(&self.usage_repo),
            ctx: repo_ctx(),
            project_id: api_key.project_id.clone(),
        };
        let api_key_id = api_key.id.to_string();
        let quota_service = QuotaService::new();
        let usages = quota_service
            .profile_quota_usages(&bridge, &api_key_id, &profile_pairs, Utc::now(), offset)
            .await
            .map_err(ConduitError::from)?;

        Ok(usages
            .into_iter()
            .map(|u| GqlProfileQuotaUsage {
                profile_name: u.profile_name,
                quota: core_quota_to_gql(u.quota),
                window: GqlQuotaWindow {
                    start: u.window.start,
                    end: u.window.end,
                },
                usage: GqlQuotaUsage {
                    // Go casts the u64 counters to `int`; the GraphQL Int is i64.
                    request_count: u.usage.requests as i64,
                    total_tokens: u.usage.tokens as i64,
                    total_cost: u.usage.cost,
                },
            })
            .collect())
    }
}

// ===========================================================================
// Constructor
// ===========================================================================

/// Build a production [`OpenApiServices`] bundle from backend repositories.
///
/// This is the single business-wiring path used by PostgreSQL;
/// backend modules only construct their repository implementations.
pub fn build_openapi_services_from_repos(
    api_key_repo: Arc<dyn ApiKeyRepo>,
    template_repo: Arc<dyn ProfileTemplateRepo>,
    usage_repo: Arc<dyn UsageRepo>,
    key_prefix: String,
) -> OpenApiServices {
    let api_keys: Arc<dyn OpenApiApiKeyService> = Arc::new(OpenApiApiKeyAdapter::new(
        Arc::clone(&api_key_repo),
        key_prefix,
    ));
    let templates: Arc<dyn OpenApiApiKeyProfileTemplateService> = Arc::new(
        OpenApiProfileTemplateAdapter::new(template_repo, api_key_repo),
    );
    let quota: Arc<dyn OpenApiQuotaService> = Arc::new(OpenApiQuotaAdapter::new(usage_repo));

    OpenApiServices {
        api_keys,
        templates,
        quota,
    }
}

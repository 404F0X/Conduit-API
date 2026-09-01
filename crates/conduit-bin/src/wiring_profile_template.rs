//! ADPT-PROFILE-TEMPLATE — host adapter wiring the admin GraphQL
//! **APIKeyProfileTemplate** domain to backend-independent
//! [`ProfileTemplateRepo`] and [`ApiKeyRepo`] implementations.
//!
//! Backs the two host-injected traits declared in
//! `crates/conduit-admin-graphql/src/profile_template.rs`:
//!   - [`ProfileTemplateQueryServices`]    — `Query.apiKeyProfileTemplates`.
//!   - [`ProfileTemplateMutationServices`] — `createApiKeyProfileTemplate` /
//!     `updateApiKeyProfileTemplate` / `deleteApiKeyProfileTemplate` /
//!     `loadApiKeyProfileTemplate`.
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//!   - `Query.apiKeyProfileTemplates` (`ent.resolvers.go:310`): thin ent
//!     `Paginate` with the `CREATED_AT → Default<...>Order` (order-by-id)
//!     remap already lowered by the crate layer into
//!     `APIKeyProfileTemplateOrderSelection`. Go applies no project filter for
//!     globally authorized admins. Conduit API additionally carries the
//!     resolver's `AdminAccessScope`: project-only callers use the repository's
//!     project-keyed query, while global callers retain the cross-project
//!     projection. The adapter contains no database-specific SQL.
//!   - `createApiKeyProfileTemplate` →
//!     `biz.APIKeyProfileTemplateService.CreateTemplate`
//!     (`biz/api_key_profile_template.go:34`): `profile.Name` is FORCED to
//!     `input.Name` before persisting; a `(project_id, name)` constraint hit
//!     surfaces as `xerrors.DuplicateNameError("Template", name)` — the
//!     crate's `ProfileTemplateServiceError::DuplicateName`.
//!   - `updateApiKeyProfileTemplate` → `UpdateTemplate` (biz:114): partial
//!     merge; when a profile is supplied its name is `input.Name` if non-nil,
//!     else the EXISTING template name (biz:127-131); constraint hit →
//!     duplicate-name with `lo.FromPtr(input.Name)` (empty when no rename).
//!   - `deleteApiKeyProfileTemplate` → `DeleteTemplate` (biz:156): get →
//!     DeleteOneID (SoftDeleteMixin intercepts into a soft delete) → return
//!     the PRE-delete snapshot. This adapter returns the row fetched before
//!     the soft delete, exactly as Go does.
//!   - `loadApiKeyProfileTemplate` → `LoadTemplate` (biz:181): get template +
//!     API key → same-project guard ("template and API key must belong to the
//!     same project") → clone the template profile ("template has no profile"
//!     when nil) → candidate name = profile name, or the template name when
//!     empty → `resolveProfileNameConflict` (" (i)" suffix probing, biz:235)
//!     → append to `apiKey.Profiles` → save the API key.
//!
//! ## `profile` JSON round-trip
//! The repo stores the `profile` column as raw `serde_json::Value`; this
//! adapter round-trips it through the canonical
//! `conduit_core::objects::apikey::APIKeyProfile` (whose serde tags mirror the
//! Go `objects.APIKeyProfile` on-disk layout exactly — camelCase +
//! `channelIDs`/`modelIDs` acronym renames + omitempty), then maps core ↔
//! GraphQL shapes. The core/GraphQL conversion helpers mirror the private
//! functions in `wiring_apikey.rs` (they are not exported; duplicating them
//! here keeps this file self-contained, per the one-file task convention).
//!
//! ## GraphQL id encoding
//! Node ids on the wire are Conduit API GUIDs (`objects.GUID`, gqlgen `ID:` model
//! binding — `gqlgen.yml:76-77`): templates encode as
//! `gid://conduit/APIKeyProfileTemplate/<n>` (ent implementors table
//! `gql_node.go:55`), projects/users/api-keys as
//! `gid://conduit/{Project,User,APIKey}/<n>` (same shaping as
//! `wiring_apikey.rs`). Inbound ids accept the GUID form OR a bare numeric
//! (mirroring `GUID.UnmarshalGQL` + the sibling-adapter precedent).
//!
//! ## `where` predicate coverage (Query.apiKeyProfileTemplates)
//! FULL coverage of the crate's `APIKeyProfileTemplateWhereInput`:
//! `not`/`and`/`or` recursion, the `id` / `createdAt` / `updatedAt`
//! predicate families, the `name` / `description` string families (incl.
//! fold variants), the `projectID` family, and `hasProject` (the ent column
//! is NOT NULL, so `true` matches every live row and `false` none). The
//! `hasProjectWith` edge filter is absent from the Rust input type (pending
//! ProjectWhereInput — crate module doc), so nothing here is deferred.
//!
//! ## Pagination
//! Relay forward pagination over the crate's offset-cursor scheme
//! (`connection_from_offset_page`); `before`/`last` are not used by the admin
//! frontend and are ignored — same documented convention as
//! `wiring_prompt.rs` / `wiring_apikey.rs`. A malformed `after` degrades to
//! offset 0 rather than failing the query.

use std::sync::Arc;

use async_graphql::ID;
use async_trait::async_trait;

use conduit_admin_graphql::apikey::{
    APIKey, APIKeyProfile, APIKeyProfileInput, APIKeyProfiles, APIKeyQuota,
    APIKeyQuotaCalendarDuration, APIKeyQuotaCalendarDurationUnit, APIKeyQuotaInput,
    APIKeyQuotaPastDuration, APIKeyQuotaPastDurationUnit, APIKeyQuotaPeriod, APIKeyQuotaPeriodType,
    APIKeyStatus, APIKeyType, ChannelTagsMatchMode,
};
use conduit_admin_graphql::channel::{ModelMapping as GqlModelMapping, OrderDirection};
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::pagination::{connection_from_offset_page, decode_offset_cursor};
use conduit_admin_graphql::policy::AdminAccessScope;
use conduit_admin_graphql::profile_template::{
    APIKeyProfileTemplate, APIKeyProfileTemplateConnection, APIKeyProfileTemplateConnectionArgs,
    APIKeyProfileTemplateEdge, APIKeyProfileTemplateOrderTerm, APIKeyProfileTemplateWhereInput,
    CreateAPIKeyProfileTemplateInput, LoadApiKeyProfileTemplateInput,
    ProfileTemplateMutationServices, ProfileTemplateQueryServices, ProfileTemplateServiceError,
    UpdateAPIKeyProfileTemplateInput,
};
use conduit_admin_graphql::scalars::{CursorScalar, DecimalScalar, TimeScalar};
use conduit_core::objects::apikey::{
    APIKeyProfile as CoreProfile, APIKeyProfiles as CoreProfiles, APIKeyQuota as CoreQuota,
    APIKeyQuotaCalendarDuration as CoreCalDuration, APIKeyQuotaPastDuration as CorePastDuration,
    APIKeyQuotaPeriod as CoreQuotaPeriod, ModelMapping as CoreModelMapping,
};
use conduit_db::repo::ApiKeyRepo;
use conduit_db::repo::api_key_repo::{
    ListApiKeysQuery, UpdateApiKeyInput as RepoUpdateApiKeyInput,
};
use conduit_db::repo::profile_template_repo::{
    ApiKeyProfileTemplateRow, CreateProfileTemplateInput as RepoCreateTemplateInput,
    ProfileTemplateRepo, UpdateProfileTemplateInput as RepoUpdateTemplateInput,
};
use conduit_db::row::ApiKeyRow;
use conduit_db::{PolicyContext, Principal, ProjectScope, RepoError, RequestContext, scope_slug};

// ===========================================================================
// Adapter
// ===========================================================================

/// Concrete host adapter implementing both profile-template service traits.
/// The template repo backs the CRUD surface; the api-key repo backs the
/// `loadApiKeyProfileTemplate` append-to-key mutation (Go `LoadTemplate`
/// touches both entities inside one service call).
pub struct ProfileTemplateServiceAdapter {
    template_repo: Arc<dyn ProfileTemplateRepo>,
    api_key_repo: Arc<dyn ApiKeyRepo>,
}

struct ProfileTemplateDbContext {
    context: RequestContext,
    authorized_project_id: Option<String>,
}

impl ProfileTemplateServiceAdapter {
    pub fn new(
        template_repo: Arc<dyn ProfileTemplateRepo>,
        api_key_repo: Arc<dyn ApiKeyRepo>,
    ) -> Self {
        Self {
            template_repo,
            api_key_repo,
        }
    }

    /// Lower the already-authorized GraphQL capability into the narrower DB
    /// policy model. The synthetic user is intentionally not an owner and is
    /// never a System/Test principal: project-scoped access remains confined
    /// to the selected project by the repository's project guard. Global is
    /// only constructed from `AdminAccessScope::Global`, which the resolver
    /// derives from an authenticated owner or an equivalent global grant.
    fn db_context(
        access: &AdminAccessScope,
        write: bool,
    ) -> Result<ProfileTemplateDbContext, ProfileTemplateServiceError> {
        let authorized_project_id = match access {
            AdminAccessScope::Global => None,
            AdminAccessScope::Project(project_id) => {
                Some(decode_db_id(project_id).ok_or_else(|| {
                    ProfileTemplateServiceError::PermissionDenied(
                        "authorized project id is invalid".to_owned(),
                    )
                })?)
            }
        };
        let project_scope = authorized_project_id
            .as_ref()
            .map_or(ProjectScope::All, |project_id| {
                ProjectScope::project_ids([project_id.as_str()])
            });
        let mut scope_slugs = vec![scope_slug::READ_API_KEYS];
        if write {
            scope_slugs.push(scope_slug::WRITE_API_KEYS);
        }
        let principal = Principal::user("admin-graphql/profile-template", project_scope)
            .with_scope_slugs(scope_slugs);
        let mut context = RequestContext::new(PolicyContext::new(principal));
        if let Some(project_id) = &authorized_project_id {
            context = context.with_project_id(project_id.clone());
        }
        Ok(ProfileTemplateDbContext {
            context,
            authorized_project_id,
        })
    }

    async fn load_template_rows(
        &self,
        scoped: &ProfileTemplateDbContext,
    ) -> Result<Vec<ApiKeyProfileTemplateRow>, ProfileTemplateServiceError> {
        match scoped.authorized_project_id.as_deref() {
            Some(project_id) => {
                self.template_repo
                    .list_profile_templates(&scoped.context, project_id)
                    .await
            }
            None => {
                self.template_repo
                    .list_all_profile_templates_unchecked(&scoped.context)
                    .await
            }
        }
        .map_err(|error| ProfileTemplateServiceError::Query(error.to_string()))
    }

    /// Find one live template inside the authorized boundary. Project access
    /// uses the repository's `(project_id, id)` lookup so a foreign id is
    /// indistinguishable from a missing row. Only a previously authorized
    /// global capability may use the id-only lookup.
    async fn find_template_row(
        &self,
        scoped: &ProfileTemplateDbContext,
        db_id: &str,
    ) -> Result<Option<ApiKeyProfileTemplateRow>, String> {
        // decode_db_id validated the id as numeric already; a non-parse here
        // simply means "no such row" in the integer-keyed table.
        let Ok(id_i) = db_id.parse::<i64>() else {
            return Ok(None);
        };
        match scoped.authorized_project_id.as_deref() {
            Some(project_id) => {
                self.template_repo
                    .find_profile_template(&scoped.context, project_id, &id_i.to_string())
                    .await
            }
            None => {
                self.template_repo
                    .find_profile_template_by_id_unchecked(&scoped.context, &id_i.to_string())
                    .await
            }
        }
        .map_err(|error| error.to_string())
    }

    /// Resolve an API key without widening a project capability. The API-key
    /// repository has no `(project_id, id)` lookup, so project-scoped callers
    /// enumerate only that project's checked list and never observe a foreign
    /// row. Global callers may use the id lookup because their capability is
    /// already global.
    async fn find_api_key_row(
        &self,
        scoped: &ProfileTemplateDbContext,
        api_key_id: &str,
    ) -> Result<Option<ApiKeyRow>, String> {
        let Some(project_id) = scoped.authorized_project_id.as_deref() else {
            return self
                .api_key_repo
                .find_api_key_by_id(&scoped.context, api_key_id)
                .await
                .map_err(|error| error.to_string());
        };

        const PAGE_SIZE: u32 = 500;
        let mut offset = 0_u32;
        loop {
            let page = self
                .api_key_repo
                .list_api_keys_by_project(
                    &scoped.context,
                    project_id,
                    &ListApiKeysQuery {
                        limit: PAGE_SIZE,
                        offset,
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            if let Some(row) = page.rows.into_iter().find(|row| row.id == api_key_id) {
                return Ok(Some(row));
            }
            if !page.has_more {
                return Ok(None);
            }
            let Some(next_offset) = offset.checked_add(PAGE_SIZE) else {
                return Ok(None);
            };
            offset = next_offset;
        }
    }
}

// ===========================================================================
// GUID id encode / decode
// ===========================================================================

fn template_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/APIKeyProfileTemplate/{id}"))
}

fn project_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/Project/{id}"))
}

fn apikey_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/APIKey/{id}"))
}

fn user_gid(id: &str) -> ID {
    ID::from(format!("gid://conduit/User/{id}"))
}

/// Decode a GraphQL `ID!` (`gid://conduit/<Type>/<n>` wire form or a bare
/// numeric id) into the numeric DB-id string the repo expects. Mirrors Go
/// `GUID.UnmarshalGQL` (via the crate's `node::parse_guid`); anything else is
/// treated as "no such row" by the caller.
fn decode_db_id(raw: &str) -> Option<String> {
    if let Ok(guid) = parse_guid(raw) {
        return Some(guid.id.to_string());
    }
    if raw.parse::<i64>().is_ok() {
        return Some(raw.to_string());
    }
    None
}

/// Decode a where-predicate `ID` into the numeric row id for comparison.
fn gql_id_to_i64(id: &ID) -> Option<i64> {
    decode_db_id(id.as_str()).and_then(|s| s.parse::<i64>().ok())
}

// ===========================================================================
// Enum <-> wire-string maps (wire strings are the Go/GraphQL bound values;
// mirrors the private maps in wiring_apikey.rs)
// ===========================================================================

fn key_type_from_wire(s: &str) -> APIKeyType {
    match s {
        "service_account" => APIKeyType::ServiceAccount,
        "noauth" => APIKeyType::Noauth,
        _ => APIKeyType::User,
    }
}

fn status_from_wire(s: &str) -> APIKeyStatus {
    match s {
        "disabled" => APIKeyStatus::Disabled,
        "archived" => APIKeyStatus::Archived,
        _ => APIKeyStatus::Enabled,
    }
}

fn gql_tags_mode_to_wire(m: ChannelTagsMatchMode) -> &'static str {
    match m {
        ChannelTagsMatchMode::Any => "any",
        ChannelTagsMatchMode::All => "all",
        ChannelTagsMatchMode::None => "none",
    }
}

fn wire_to_gql_tags_mode(s: &str) -> Option<ChannelTagsMatchMode> {
    match s {
        "any" => Some(ChannelTagsMatchMode::Any),
        "all" => Some(ChannelTagsMatchMode::All),
        "none" => Some(ChannelTagsMatchMode::None),
        _ => None,
    }
}

fn gql_period_type_to_wire(t: APIKeyQuotaPeriodType) -> &'static str {
    match t {
        APIKeyQuotaPeriodType::AllTime => "all_time",
        APIKeyQuotaPeriodType::PastDuration => "past_duration",
        APIKeyQuotaPeriodType::CalendarDuration => "calendar_duration",
    }
}

fn wire_to_gql_period_type(s: &str) -> APIKeyQuotaPeriodType {
    match s {
        "past_duration" => APIKeyQuotaPeriodType::PastDuration,
        "calendar_duration" => APIKeyQuotaPeriodType::CalendarDuration,
        _ => APIKeyQuotaPeriodType::AllTime,
    }
}

fn gql_past_unit_to_wire(u: APIKeyQuotaPastDurationUnit) -> &'static str {
    match u {
        APIKeyQuotaPastDurationUnit::Minute => "minute",
        APIKeyQuotaPastDurationUnit::Hour => "hour",
        APIKeyQuotaPastDurationUnit::Day => "day",
    }
}

fn wire_to_gql_past_unit(s: &str) -> APIKeyQuotaPastDurationUnit {
    match s {
        "hour" => APIKeyQuotaPastDurationUnit::Hour,
        "day" => APIKeyQuotaPastDurationUnit::Day,
        _ => APIKeyQuotaPastDurationUnit::Minute,
    }
}

fn gql_cal_unit_to_wire(u: APIKeyQuotaCalendarDurationUnit) -> &'static str {
    match u {
        APIKeyQuotaCalendarDurationUnit::Day => "day",
        APIKeyQuotaCalendarDurationUnit::Month => "month",
    }
}

fn wire_to_gql_cal_unit(s: &str) -> APIKeyQuotaCalendarDurationUnit {
    match s {
        "month" => APIKeyQuotaCalendarDurationUnit::Month,
        _ => APIKeyQuotaCalendarDurationUnit::Day,
    }
}

// ===========================================================================
// Profile conversions: GraphQL input → core (write path), core → GraphQL
// output (read path). The core objects' serde tags produce the exact Go
// on-disk JSON (`objects.APIKeyProfile` camelCase + acronym renames).
// ===========================================================================

fn profile_input_to_core(p: APIKeyProfileInput) -> CoreProfile {
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
        quota: p.quota.map(quota_input_to_core),
        load_balance_strategy: p.load_balance_strategy,
        max_concurrent_requests: p
            .max_concurrent_requests
            .and_then(|value| u32::try_from(value).ok()),
        channel_ids: p.channel_ids.unwrap_or_default(),
        channel_tags: p.channel_tags.unwrap_or_default(),
        channel_tags_match_mode: p
            .channel_tags_match_mode
            .map(|m| gql_tags_mode_to_wire(m).to_owned()),
        model_ids: p.model_ids.unwrap_or_default(),
        valid_from: p.valid_from.map(|value| value.0),
        valid_until: p.valid_until.map(|value| value.0),
    }
}

fn quota_input_to_core(q: APIKeyQuotaInput) -> CoreQuota {
    CoreQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(|c| c.0),
        period: CoreQuotaPeriod {
            r#type: gql_period_type_to_wire(q.period.period_type).to_owned(),
            past_duration: q.period.past_duration.map(|pd| CorePastDuration {
                value: pd.value,
                unit: gql_past_unit_to_wire(pd.unit).to_owned(),
            }),
            calendar_duration: q.period.calendar_duration.map(|cd| CoreCalDuration {
                unit: gql_cal_unit_to_wire(cd.unit).to_owned(),
            }),
        },
    }
}

fn core_profile_to_gql(p: CoreProfile) -> APIKeyProfile {
    APIKeyProfile {
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
        valid_from: p.valid_from.map(TimeScalar),
        valid_until: p.valid_until.map(TimeScalar),
        quota: p.quota.map(core_quota_to_gql),
        load_balance_strategy: p.load_balance_strategy,
        max_concurrent_requests: p.max_concurrent_requests.map(i64::from),
    }
}

fn core_quota_to_gql(q: CoreQuota) -> APIKeyQuota {
    APIKeyQuota {
        requests: q.requests,
        total_tokens: q.total_tokens,
        cost: q.cost.map(DecimalScalar),
        period: APIKeyQuotaPeriod {
            period_type: wire_to_gql_period_type(&q.period.r#type),
            past_duration: q.period.past_duration.map(|pd| APIKeyQuotaPastDuration {
                value: pd.value,
                unit: wire_to_gql_past_unit(&pd.unit),
            }),
            calendar_duration: q
                .period
                .calendar_duration
                .map(|cd| APIKeyQuotaCalendarDuration {
                    unit: wire_to_gql_cal_unit(&cd.unit),
                }),
        },
    }
}

fn core_profiles_to_gql(core: CoreProfiles) -> APIKeyProfiles {
    let profiles: Vec<APIKeyProfile> = core.profiles.into_iter().map(core_profile_to_gql).collect();
    APIKeyProfiles {
        active_profile: core.active_profile,
        profiles: if profiles.is_empty() {
            None
        } else {
            Some(profiles)
        },
    }
}

// ===========================================================================
// Row → GraphQL conversions
// ===========================================================================

/// Convert a template row into the GraphQL output type. The `profile` JSON
/// column round-trips through the canonical `objects.APIKeyProfile` (Go
/// `*objects.APIKeyProfile` — SQL NULL / JSON `null` maps to GraphQL null;
/// a malformed value degrades to the empty profile rather than failing the
/// whole read, same lenient-read convention as `wiring_apikey.rs`).
pub(crate) fn template_row_to_gql(row: ApiKeyProfileTemplateRow) -> APIKeyProfileTemplate {
    let profile = row.profile.and_then(|v| {
        if v.is_null() {
            None
        } else {
            let core: CoreProfile = serde_json::from_value(v).unwrap_or_default();
            Some(core_profile_to_gql(core))
        }
    });
    APIKeyProfileTemplate {
        id: template_gid(&row.id),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        name: row.name,
        description: row.description,
        project_id: project_gid(&row.project_id),
        profile,
    }
}

/// Convert an [`ApiKeyRow`] into the GraphQL [`APIKey`] (the
/// `loadApiKeyProfileTemplate` return type). Full `key` echoed — no masking
/// on the Go admin surface (see `wiring_apikey.rs` module doc). Mirrors the
/// private `wiring_apikey::row_to_gql`.
fn api_key_row_to_gql(row: ApiKeyRow) -> APIKey {
    // A malformed profiles JSON degrades to the empty struct rather than
    // failing the read.
    let core_profiles: CoreProfiles =
        serde_json::from_value(row.profiles.clone()).unwrap_or_default();
    APIKey {
        id: apikey_gid(&row.id),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        user_id: row.user_id.as_deref().map(user_gid),
        project_id: project_gid(&row.project_id),
        key: row.key,
        name: row.name,
        key_type: key_type_from_wire(&row.key_type),
        status: status_from_wire(&row.status),
        scopes: Some(row.scopes),
        // Go `APIKey.Profiles` is a value type (never null); always present.
        profiles: Some(core_profiles_to_gql(core_profiles)),
    }
}

// ===========================================================================
// ProfileTemplateQueryServices — Query.apiKeyProfileTemplates
// ===========================================================================

#[async_trait]
impl ProfileTemplateQueryServices for ProfileTemplateServiceAdapter {
    async fn api_key_profile_templates_with_access(
        &self,
        access: &AdminAccessScope,
        args: APIKeyProfileTemplateConnectionArgs,
    ) -> Result<APIKeyProfileTemplateConnection, ProfileTemplateServiceError> {
        let scoped = Self::db_context(access, false)?;
        let rows = self.load_template_rows(&scoped).await?;

        // `where` filter (in-memory; FULL predicate coverage — module doc).
        let mut rows: Vec<ApiKeyProfileTemplateRow> = match &args.where_filter {
            Some(w) => rows
                .into_iter()
                .filter(|r| template_row_matches_where(r, w))
                .collect(),
            None => rows,
        };

        // Baseline order: ent `DefaultAPIKeyProfileTemplateOrder` (by id ASC) —
        // the Paginate default. The repo returned per-project id-ASC batches
        // concatenated across projects, so a global re-sort is required.
        rows.sort_by_key(|r| r.id.parse::<i64>().unwrap_or(i64::MAX));

        // Explicit ordering: the crate already lowered `CREATED_AT` → `Id`
        // (ent.resolvers.go:310); `UPDATED_AT` maps one-to-one.
        if let Some(selection) = &args.order_by {
            rows.sort_by(|a, b| {
                let ordering = match selection.term {
                    APIKeyProfileTemplateOrderTerm::Id => {
                        a.id.parse::<i64>()
                            .unwrap_or(i64::MAX)
                            .cmp(&b.id.parse::<i64>().unwrap_or(i64::MAX))
                    }
                    APIKeyProfileTemplateOrderTerm::UpdatedAt => a.updated_at.cmp(&b.updated_at),
                };
                match selection.direction {
                    OrderDirection::Asc => ordering,
                    OrderDirection::Desc => ordering.reverse(),
                }
            });
        }

        let total_count = rows.len() as i64;
        let nodes: Vec<APIKeyProfileTemplate> = rows.into_iter().map(template_row_to_gql).collect();

        // Relay forward pagination over the offset-cursor scheme; `before` /
        // `last` ignored (sibling-adapter convention, module doc). A malformed
        // `after` degrades to offset 0.
        let start_offset = args
            .after
            .as_deref()
            .and_then(|c| decode_offset_cursor(c).ok())
            .map(|o| o + 1)
            .unwrap_or(0);
        let start = usize::try_from(start_offset).unwrap_or(0).min(nodes.len());
        let windowed = nodes[start..].to_vec();
        let page_size = match args.first {
            Some(first) => usize::try_from(first).unwrap_or(0),
            None => windowed.len(),
        };
        let connection = connection_from_offset_page(windowed, start_offset, page_size);

        Ok(APIKeyProfileTemplateConnection {
            edges: Some(
                connection
                    .edges
                    .into_iter()
                    .map(|edge| {
                        Some(APIKeyProfileTemplateEdge {
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

// ===========================================================================
// ProfileTemplateMutationServices — create / update / delete / load
// ===========================================================================

#[async_trait]
impl ProfileTemplateMutationServices for ProfileTemplateServiceAdapter {
    async fn create_api_key_profile_template_with_access(
        &self,
        access: &AdminAccessScope,
        input: CreateAPIKeyProfileTemplateInput,
        profile: APIKeyProfileInput,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
        let scoped = Self::db_context(access, true)?;
        let project_db = decode_db_id(input.project_id.as_str())
            .ok_or_else(|| ProfileTemplateServiceError::Create("invalid project id".to_owned()))?;
        if !access.allows_project(&project_db) {
            return Err(ProfileTemplateServiceError::PermissionDenied(
                "cannot create a template outside the authorized project".to_owned(),
            ));
        }

        // biz/api_key_profile_template.go:37-39 — profile.Name is FORCED to
        // the template name before persisting.
        let mut core_profile = profile_input_to_core(profile);
        core_profile.name = input.name.clone();
        let profile_json = serde_json::to_value(&core_profile)
            .map_err(|e| ProfileTemplateServiceError::Create(e.to_string()))?;

        let row = self
            .template_repo
            .create_profile_template(
                &scoped.context,
                RepoCreateTemplateInput {
                    project_id: project_db,
                    name: input.name.clone(),
                    description: input.description,
                    profile: Some(profile_json),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await
            .map_err(|e| match e {
                // Go: ent.IsConstraintError → DuplicateNameError("Template", name).
                RepoError::NameConflict => ProfileTemplateServiceError::DuplicateName(input.name),
                other => ProfileTemplateServiceError::Create(other.to_string()),
            })?;
        Ok(template_row_to_gql(row))
    }

    async fn update_api_key_profile_template_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        input: UpdateAPIKeyProfileTemplateInput,
        profile: Option<APIKeyProfileInput>,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
        let scoped = Self::db_context(access, true)?;
        let not_found = || {
            ProfileTemplateServiceError::Update(ProfileTemplateServiceError::NotFound.to_string())
        };
        let db_id = decode_db_id(id).ok_or_else(not_found)?;

        // The existing row is needed to resolve the owning project (the repo
        // keys by `(project_id, id)`) and, when a profile is supplied, the
        // name fallback (Go biz:121-131 gets the template inside the txn).
        let existing = self
            .find_template_row(&scoped, &db_id)
            .await
            .map_err(ProfileTemplateServiceError::Update)?
            .ok_or_else(not_found)?;

        // biz:127-131 — profile.Name = input.Name when non-nil, else the
        // existing template name.
        let profile_json = match profile {
            Some(p) => {
                let mut core = profile_input_to_core(p);
                core.name = input.name.clone().unwrap_or_else(|| existing.name.clone());
                Some(
                    serde_json::to_value(&core)
                        .map_err(|e| ProfileTemplateServiceError::Update(e.to_string()))?,
                )
            }
            None => None,
        };

        let row = self
            .template_repo
            .update_profile_template(
                &scoped.context,
                &existing.project_id,
                &db_id,
                RepoUpdateTemplateInput {
                    name: input.name.clone(),
                    description: input.description,
                    profile: profile_json,
                    updated_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await
            .map_err(|e| match e {
                // Go biz:137-141 — lo.FromPtr(input.Name) (empty when no rename).
                RepoError::NameConflict => {
                    ProfileTemplateServiceError::DuplicateName(input.name.unwrap_or_default())
                }
                RepoError::NotFound(_) => not_found(),
                other => ProfileTemplateServiceError::Update(other.to_string()),
            })?;
        Ok(template_row_to_gql(row))
    }

    async fn delete_api_key_profile_template_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
        let scoped = Self::db_context(access, true)?;
        let not_found = || {
            ProfileTemplateServiceError::Delete(ProfileTemplateServiceError::NotFound.to_string())
        };
        let db_id = decode_db_id(id).ok_or_else(not_found)?;

        // Go biz:156-179 — get first, then DeleteOneID; the PRE-delete
        // snapshot is what the mutation returns.
        let existing = self
            .find_template_row(&scoped, &db_id)
            .await
            .map_err(ProfileTemplateServiceError::Delete)?
            .ok_or_else(not_found)?;

        self.template_repo
            .soft_delete_profile_template(
                &scoped.context,
                &existing.project_id,
                &db_id,
                chrono::Utc::now().to_rfc3339(),
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => not_found(),
                other => ProfileTemplateServiceError::Delete(other.to_string()),
            })?;
        Ok(template_row_to_gql(existing))
    }

    async fn load_api_key_profile_template_with_access(
        &self,
        access: &AdminAccessScope,
        input: LoadApiKeyProfileTemplateInput,
    ) -> Result<APIKey, ProfileTemplateServiceError> {
        let scoped = Self::db_context(access, true)?;
        // Error surfaces mirror the crate's in-memory double (the golden
        // behavior): a missing template wraps the ent template-not-found
        // string, a missing key wraps "ent: apikey not found".
        let template_not_found =
            || ProfileTemplateServiceError::Load(ProfileTemplateServiceError::NotFound.to_string());
        let key_not_found =
            || ProfileTemplateServiceError::Load("ent: apikey not found".to_owned());

        let template_db =
            decode_db_id(input.template_id.as_str()).ok_or_else(template_not_found)?;
        let api_key_db = decode_db_id(input.api_key_id.as_str()).ok_or_else(key_not_found)?;

        let template = self
            .find_template_row(&scoped, &template_db)
            .await
            .map_err(ProfileTemplateServiceError::Load)?
            .ok_or_else(template_not_found)?;

        let api_key = self
            .find_api_key_row(&scoped, &api_key_db)
            .await
            .map_err(ProfileTemplateServiceError::Load)?
            .ok_or_else(key_not_found)?;

        // biz:196-198 — same-project guard.
        if template.project_id != api_key.project_id {
            return Err(ProfileTemplateServiceError::CrossProjectLoad);
        }

        // biz:200-203 — a nil template profile is an error.
        let template_profile: CoreProfile = match template.profile {
            Some(v) if !v.is_null() => serde_json::from_value(v)
                .map_err(|e| ProfileTemplateServiceError::Load(e.to_string()))?,
            _ => return Err(ProfileTemplateServiceError::TemplateProfileMissing),
        };

        // biz:205-208 — nil key profiles start from the empty struct.
        let mut existing_profiles: CoreProfiles =
            serde_json::from_value(api_key.profiles.clone()).unwrap_or_default();

        // biz:210-217 — candidate name is the profile name (or the template
        // name when empty), then resolveProfileNameConflict (" (i)" probing).
        let mut candidate = template_profile.name.clone();
        if candidate.is_empty() {
            candidate = template.name.clone();
        }
        let resolved = resolve_profile_name_conflict(&existing_profiles.profiles, candidate);

        let mut new_profile = template_profile;
        new_profile.name = resolved;
        existing_profiles.profiles.push(new_profile);

        let profiles_json = serde_json::to_value(&existing_profiles)
            .map_err(|e| ProfileTemplateServiceError::Load(e.to_string()))?;

        // biz:219-224 — persist the appended profile list on the API key.
        let row = self
            .api_key_repo
            .update_api_key(
                &scoped.context,
                &api_key_db,
                RepoUpdateApiKeyInput {
                    profiles: Some(profiles_json),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| match e {
                RepoError::NotFound(_) => key_not_found(),
                other => ProfileTemplateServiceError::Load(other.to_string()),
            })?;
        Ok(api_key_row_to_gql(row))
    }
}

/// Mirrors Go `resolveProfileNameConflict` (biz/api_key_profile_template.go:235):
/// the candidate name is returned unchanged when free; otherwise `"{name} (i)"`
/// is probed for i = 1, 2, ... until a free name is found.
fn resolve_profile_name_conflict(existing: &[CoreProfile], new_name: String) -> String {
    let names: std::collections::HashSet<&str> = existing.iter().map(|p| p.name.as_str()).collect();
    if !names.contains(new_name.as_str()) {
        return new_name;
    }
    let mut i = 1u64;
    loop {
        let candidate = format!("{new_name} ({i})");
        if !names.contains(candidate.as_str()) {
            return candidate;
        }
        i += 1;
    }
}

// ===========================================================================
// `where` predicate evaluation (Query.apiKeyProfileTemplates)
// ===========================================================================

/// Whether a template row satisfies an `APIKeyProfileTemplateWhereInput`
/// predicate tree. `not`/`and`/`or` recurse; an empty `and` matches (ent
/// semantics) and an empty `or` is ignored so it never blacks out the result.
fn template_row_matches_where(
    row: &ApiKeyProfileTemplateRow,
    w: &APIKeyProfileTemplateWhereInput,
) -> bool {
    if let Some(inner) = &w.not
        && template_row_matches_where(row, inner)
    {
        return false;
    }
    if let Some(list) = &w.and
        && !list.iter().all(|c| template_row_matches_where(row, c))
    {
        return false;
    }
    if let Some(list) = &w.or
        && !list.is_empty()
        && !list.iter().any(|c| template_row_matches_where(row, c))
    {
        return false;
    }

    let row_id = row.id.parse::<i64>().unwrap_or_default();
    let project_id = row.project_id.parse::<i64>().unwrap_or_default();

    // id predicates (GUID or bare-numeric wire ids compare by row id).
    if !id_family(
        row_id,
        &w.id,
        &w.id_neq,
        &w.id_in,
        &w.id_not_in,
        &w.id_gt,
        &w.id_gte,
        &w.id_lt,
        &w.id_lte,
    ) {
        return false;
    }

    // created_at / updated_at predicates.
    if !time_family(
        row.created_at,
        &w.created_at,
        &w.created_at_neq,
        &w.created_at_in,
        &w.created_at_not_in,
        &w.created_at_gt,
        &w.created_at_gte,
        &w.created_at_lt,
        &w.created_at_lte,
    ) {
        return false;
    }
    if !time_family(
        row.updated_at,
        &w.updated_at,
        &w.updated_at_neq,
        &w.updated_at_in,
        &w.updated_at_not_in,
        &w.updated_at_gt,
        &w.updated_at_gte,
        &w.updated_at_lt,
        &w.updated_at_lte,
    ) {
        return false;
    }

    // name string family.
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

    // description string family.
    if !str_family(
        &row.description,
        &w.description,
        &w.description_neq,
        &w.description_in,
        &w.description_not_in,
        &w.description_gt,
        &w.description_gte,
        &w.description_lt,
        &w.description_lte,
        &w.description_contains,
        &w.description_has_prefix,
        &w.description_has_suffix,
        &w.description_equal_fold,
        &w.description_contains_fold,
    ) {
        return false;
    }

    // projectID family (eq / neq / in / notIn only — the crate input declares
    // no range predicates for the edge-backed column).
    if !id_family(
        project_id,
        &w.project_id,
        &w.project_id_neq,
        &w.project_id_in,
        &w.project_id_not_in,
        &None,
        &None,
        &None,
        &None,
    ) {
        return false;
    }

    // hasProject existence: `project_id` is NOT NULL in the ent schema, so
    // every live template has its project — `true` matches all, `false` none.
    if w.has_project == Some(false) {
        return false;
    }

    true
}

/// Evaluate the id-predicate family (eq/neq/in/notIn/gt/gte/lt/lte) against a
/// numeric row id. Predicate `ID` values decode via the GUID/bare-numeric
/// rules; an undecodable value makes a positive constraint unsatisfiable
/// (matches nothing), same as an unknown id in ent. `None` predicates are
/// skipped (AND semantics).
#[allow(clippy::too_many_arguments)]
fn id_family(
    value: i64,
    eq: &Option<ID>,
    neq: &Option<ID>,
    in_set: &Option<Vec<ID>>,
    not_in: &Option<Vec<ID>>,
    gt: &Option<ID>,
    gte: &Option<ID>,
    lt: &Option<ID>,
    lte: &Option<ID>,
) -> bool {
    if let Some(v) = eq
        && gql_id_to_i64(v) != Some(value)
    {
        return false;
    }
    if let Some(v) = neq
        && gql_id_to_i64(v) == Some(value)
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().filter_map(gql_id_to_i64).any(|x| x == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().filter_map(gql_id_to_i64).any(|x| x == value)
    {
        return false;
    }
    if let Some(v) = gt {
        match gql_id_to_i64(v) {
            Some(x) if value > x => {}
            _ => return false,
        }
    }
    if let Some(v) = gte {
        match gql_id_to_i64(v) {
            Some(x) if value >= x => {}
            _ => return false,
        }
    }
    if let Some(v) = lt {
        match gql_id_to_i64(v) {
            Some(x) if value < x => {}
            _ => return false,
        }
    }
    if let Some(v) = lte {
        match gql_id_to_i64(v) {
            Some(x) if value <= x => {}
            _ => return false,
        }
    }
    true
}

/// Evaluate the time-predicate family (eq/neq/in/notIn/gt/gte/lt/lte) against
/// a timestamp column. `None` predicates are skipped (AND semantics).
#[allow(clippy::too_many_arguments)]
fn time_family(
    value: chrono::DateTime<chrono::Utc>,
    eq: &Option<TimeScalar>,
    neq: &Option<TimeScalar>,
    in_set: &Option<Vec<TimeScalar>>,
    not_in: &Option<Vec<TimeScalar>>,
    gt: &Option<TimeScalar>,
    gte: &Option<TimeScalar>,
    lt: &Option<TimeScalar>,
    lte: &Option<TimeScalar>,
) -> bool {
    if let Some(v) = eq
        && value != v.0
    {
        return false;
    }
    if let Some(v) = neq
        && value == v.0
    {
        return false;
    }
    if let Some(list) = in_set
        && !list.iter().any(|x| x.0 == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|x| x.0 == value)
    {
        return false;
    }
    if let Some(v) = gt
        && value <= v.0
    {
        return false;
    }
    if let Some(v) = gte
        && value < v.0
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v.0
    {
        return false;
    }
    if let Some(v) = lte
        && value > v.0
    {
        return false;
    }
    true
}

/// Evaluate the full string-predicate family (eq/neq/in/notIn/gt/gte/lt/lte/
/// contains/hasPrefix/hasSuffix/equalFold/containsFold) against a column
/// value. `None` predicates are skipped (AND semantics, matching ent).
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
        && value <= v.as_str()
    {
        return false;
    }
    if let Some(v) = gte
        && value < v.as_str()
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v.as_str()
    {
        return false;
    }
    if let Some(v) = lte
        && value > v.as_str()
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

#[cfg(test)]
mod postgres_tests {
    use super::*;
    use conduit_db::repo::api_key_repo::CreateApiKeyInput as RepoCreateApiKeyInput;
    use conduit_db::{PgApiKeyRepo, PgProfileTemplateRepo};

    type TestError = Box<dyn std::error::Error>;

    #[tokio::test]
    async fn postgres_admin_profile_template_crud_and_load_round_trip() -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let api_key_repo = Arc::new(PgApiKeyRepo::new(pool.clone()));
        let template_repo: Arc<dyn ProfileTemplateRepo> =
            Arc::new(PgProfileTemplateRepo::new(pool));
        let api_key_service_repo: Arc<dyn ApiKeyRepo> = api_key_repo.clone();
        let adapter = ProfileTemplateServiceAdapter::new(template_repo, api_key_service_repo);

        let project_id = "41";
        let access_41 = AdminAccessScope::Project(project_id.to_owned());
        let access_42 = AdminAccessScope::Project("42".to_owned());
        let created = adapter
            .create_api_key_profile_template_with_access(
                &access_41,
                CreateAPIKeyProfileTemplateInput {
                    name: "Restricted".to_string(),
                    description: Some("PostgreSQL profile".to_string()),
                    project_id: ID::from(project_id),
                },
                APIKeyProfileInput {
                    name: "ignored-by-template-create".to_string(),
                    model_mappings: None,
                    channel_ids: Some(vec![7, 8]),
                    channel_tags: None,
                    channel_tags_match_mode: None,
                    model_ids: Some(vec!["public-model".to_string()]),
                    valid_from: None,
                    valid_until: None,
                    quota: None,
                    load_balance_strategy: None,
                    max_concurrent_requests: None,
                },
            )
            .await?;
        assert_eq!(created.name, "Restricted");
        assert_eq!(
            created
                .profile
                .as_ref()
                .map(|profile| profile.name.as_str()),
            Some("Restricted")
        );

        let key = api_key_repo
            .create_api_key(
                &ProfileTemplateServiceAdapter::db_context(&access_41, true)?.context,
                RepoCreateApiKeyInput {
                    id: String::new(),
                    user_id: None,
                    project_id: project_id.to_string(),
                    name: "PostgreSQL key".to_string(),
                    key: "conduit-pg-profile-template-test".to_string(),
                    key_type: "user".to_string(),
                    scopes: Vec::new(),
                    profiles: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await?;
        let foreign_key = api_key_repo
            .create_api_key(
                &ProfileTemplateServiceAdapter::db_context(&access_42, true)?.context,
                RepoCreateApiKeyInput {
                    id: String::new(),
                    user_id: None,
                    project_id: "42".to_string(),
                    name: "Foreign PostgreSQL key".to_string(),
                    key: "conduit-pg-profile-template-foreign".to_string(),
                    key_type: "user".to_string(),
                    scopes: Vec::new(),
                    profiles: None,
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await?;
        let foreign_template = adapter
            .create_api_key_profile_template_with_access(
                &access_42,
                CreateAPIKeyProfileTemplateInput {
                    name: "Foreign Restricted".to_owned(),
                    description: None,
                    project_id: ID::from("42"),
                },
                APIKeyProfileInput {
                    name: "ignored".to_owned(),
                    model_mappings: None,
                    channel_ids: None,
                    channel_tags: None,
                    channel_tags_match_mode: None,
                    model_ids: None,
                    valid_from: None,
                    valid_until: None,
                    quota: None,
                    load_balance_strategy: None,
                    max_concurrent_requests: None,
                },
            )
            .await?;
        assert!(matches!(
            adapter
                .create_api_key_profile_template_with_access(
                    &access_41,
                    CreateAPIKeyProfileTemplateInput {
                        name: "Cross Project Create".to_owned(),
                        description: None,
                        project_id: ID::from("42"),
                    },
                    APIKeyProfileInput {
                        name: "ignored".to_owned(),
                        model_mappings: None,
                        channel_ids: None,
                        channel_tags: None,
                        channel_tags_match_mode: None,
                        model_ids: None,
                        valid_from: None,
                        valid_until: None,
                        quota: None,
                        load_balance_strategy: None,
                        max_concurrent_requests: None,
                    },
                )
                .await,
            Err(ProfileTemplateServiceError::PermissionDenied(_))
        ));
        assert!(matches!(
            adapter
                .update_api_key_profile_template_with_access(
                    &access_41,
                    foreign_template.id.as_str(),
                    UpdateAPIKeyProfileTemplateInput {
                        name: Some("must-not-update".to_owned()),
                        description: None,
                    },
                    None,
                )
                .await,
            Err(ProfileTemplateServiceError::Update(message))
                if message == ProfileTemplateServiceError::NotFound.to_string()
        ));
        assert!(matches!(
            adapter
                .delete_api_key_profile_template_with_access(
                    &access_41,
                    foreign_template.id.as_str(),
                )
                .await,
            Err(ProfileTemplateServiceError::Delete(message))
                if message == ProfileTemplateServiceError::NotFound.to_string()
        ));
        assert!(matches!(
            adapter
                .load_api_key_profile_template_with_access(
                    &access_41,
                    LoadApiKeyProfileTemplateInput {
                        template_id: foreign_template.id.clone(),
                        api_key_id: ID::from(foreign_key.id.clone()),
                    },
                )
                .await,
            Err(ProfileTemplateServiceError::Load(message))
                if message == ProfileTemplateServiceError::NotFound.to_string()
        ));
        let foreign_load = adapter
            .load_api_key_profile_template_with_access(
                &AdminAccessScope::Global,
                LoadApiKeyProfileTemplateInput {
                    template_id: created.id.clone(),
                    api_key_id: ID::from(foreign_key.id),
                },
            )
            .await;
        assert!(matches!(
            foreign_load,
            Err(ProfileTemplateServiceError::CrossProjectLoad)
        ));

        let loaded = adapter
            .load_api_key_profile_template_with_access(
                &access_41,
                LoadApiKeyProfileTemplateInput {
                    template_id: created.id.clone(),
                    api_key_id: ID::from(key.id),
                },
            )
            .await?;
        let profiles = loaded
            .profiles
            .and_then(|profiles| profiles.profiles)
            .ok_or("loaded API key must contain the template profile")?;
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Restricted");
        assert_eq!(profiles[0].channel_ids.as_deref(), Some(&[7, 8][..]));

        let connection = adapter
            .api_key_profile_templates_with_access(
                &access_41,
                APIKeyProfileTemplateConnectionArgs::default(),
            )
            .await?;
        assert_eq!(connection.total_count, 1);
        adapter
            .delete_api_key_profile_template_with_access(&access_41, created.id.as_str())
            .await?;
        let connection = adapter
            .api_key_profile_templates_with_access(
                &access_41,
                APIKeyProfileTemplateConnectionArgs::default(),
            )
            .await?;
        assert_eq!(connection.total_count, 0);

        database.cleanup().await?;
        Ok(())
    }
}

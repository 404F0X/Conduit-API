//! RUST-P12-001 S07 (Pauli-the-11th) — APIKeyProfileTemplate domain slice.
//!
//! Ports the `APIKeyProfileTemplate` GraphQL domain (template management for
//! API key profiles) from the Go gqlgen schema. The Go source-of-truth:
//!
//!   - `internal/server/gql/conduit.graphql:404-407` — the custom
//!     `input LoadApiKeyProfileTemplateInput { templateID apiKeyID }`.
//!   - `internal/server/gql/conduit.graphql:866-878` — four Mutation fields:
//!     `createApiKeyProfileTemplate` / `updateApiKeyProfileTemplate` /
//!     `deleteApiKeyProfileTemplate` / `loadApiKeyProfileTemplate`.
//!   - `internal/server/gql/ent.graphql:111-129` — the
//!     `type APIKeyProfileTemplate implements Node` output type (ent-generated
//!     fields: `id`, `createdAt`, `updatedAt`, `name`, `description`,
//!     `projectID`, `profile`, `project`).
//!   - `internal/server/gql/ent.graphql:133-159` — Connection / Edge shapes.
//!   - `internal/server/gql/ent.graphql:163-179` — `APIKeyProfileTemplateOrder`
//!     / `APIKeyProfileTemplateOrderField { CREATED_AT UPDATED_AT }`.
//!   - `internal/server/gql/ent.graphql:184-...` — the ent-generated
//!     `APIKeyProfileTemplateWhereInput` predicate grammar.
//!   - `internal/server/gql/ent.graphql:1541-1551` —
//!     `input CreateAPIKeyProfileTemplateInput { name description projectID }`.
//!   - `internal/server/gql/ent.graphql:5975-5984` —
//!     `input UpdateAPIKeyProfileTemplateInput { name description }`.
//!   - `internal/server/gql/ent.graphql:3908-3938` (also 3029-3059 inside
//!     `type Project`) — `Query.apiKeyProfileTemplates(after first before last
//!     orderBy where): APIKeyProfileTemplateConnection!` (the root Query
//!     field; the Project edge variant is pending the Project slice).
//!
//! Go resolver implementations:
//!   - `conduit.resolvers.go:653-670` — the four Mutation resolvers delegate
//!     to `biz.APIKeyProfileTemplateService.CreateTemplate / UpdateTemplate /
//!     DeleteTemplate / LoadTemplate` (`biz/api_key_profile_template.go`).
//!   - `ent.resolvers.go:310-323` — `Query.apiKeyProfileTemplates` delegates
//!     to ent `Paginate`, with the same `CREATED_AT → Default<...>Order`
//!     remap Go uses for every ent connection resolver
//!     (`gql_pagination.go:413`; identical to `Query.apiKeys` /
//!     `Query.channels` / `Query.models`).
//!
//! Reuse: the `profile` field on both the OUTPUT `APIKeyProfileTemplate` and
//! the `profile` argument on `create/updateApiKeyProfileTemplate` reuse the
//! shared `crate::apikey::APIKeyProfile` / `crate::apikey::APIKeyProfileInput`
//! types (snapshot declares exactly one `APIKeyProfile` GraphQL type and one
//! `APIKeyProfileInput`).
//!
//! Naming gotcha: async-graphql pascal-cases Rust type idents
//! (`APIKeyProfileTemplate` → `ApiKeyProfileTemplate`), so EVERY GraphQL type
//! name in this file is pinned with `#[graphql(name = "...")]` — proven
//! precedent: `crates/conduit-admin-graphql/src/apikey.rs:35`.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//!   - `Project.apiKeyProfileTemplates(...)` connection (snapshot 4369+) —
//!     Project-domain edge field.
//!   - `APIKeyProfileTemplateWhereInput.hasProjectWith:
//!     [ProjectWhereInput!]` — references `ProjectWhereInput` not ported yet.
//!   - `APIKeyWhereInput.hasAPIKeyProfileTemplatesWith` /
//!     `hasAPIKeyProfileTemplates` (snapshot 4582-4583) — pending the
//!     APIKey->templates edge direction (ent reverse edge).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::apikey::{APIKey, APIKeyProfile, APIKeyProfileInput};
use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// `enum APIKeyProfileTemplateOrderField { CREATED_AT UPDATED_AT }` —
/// snapshot lines 1516-1518 (two values only; templates have no NAME
/// ordering, matching ent's generated `OrderField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyProfileTemplateOrderField")]
pub enum APIKeyProfileTemplateOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// `type APIKeyProfileTemplate implements Node` — snapshot lines 1451-1469.
///
/// All self-domain fields and the cross-domain `project: Project!` edge are
/// ported. The Go ent schema
/// (`internal/ent/schema/api_key_profile_template.go`) backs every field:
/// `id` (ent GUID → GraphQL ID), `createdAt` / `updatedAt` (Time),
/// `name` (template name, unique per project), `description` (String!,
/// defaults to "" in the column), `projectID` (set via the `project` edge in
/// Go, surfaced as a scalar here), `profile` (the embedded
/// `objects.APIKeyProfile`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfileTemplate", complex)]
pub struct APIKeyProfileTemplate {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// Template name (ent column, unique per project — see
    /// `biz/api_key_profile_template.go:49`).
    pub name: String,
    /// Template description. The Go ent column is non-nullable with a default
    /// empty string, so the GraphQL contract declares it `String!`.
    pub description: String,
    // All-caps acronym tag: `rename_all = "camelCase"` would emit `projectId`.
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// Embedded profile payload (`objects.APIKeyProfile` in Go). Reuses the
    /// shared `crate::apikey::APIKeyProfile` GraphQL type — the snapshot
    /// declares exactly one `APIKeyProfile` type.
    pub profile: Option<APIKeyProfile>,
}

#[ComplexObject]
impl APIKeyProfileTemplate {
    async fn project(&self, ctx: &Context<'_>) -> Result<crate::project::Project, String> {
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_PROJECTS)
            .map_err(|error| error.to_string())?;
        let services = crate::project::project_query_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::READ_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        let connection = services
            .projects_with_access(
                &access,
                crate::project::ProjectConnectionArgs {
                    first: Some(1),
                    where_filter: Some(crate::project::ProjectWhereInput {
                        id: Some(self.project_id.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| err.to_string())?;

        connection
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node)
            .ok_or_else(|| "API key profile template's project was not found".to_string())
    }
}

/// `type APIKeyProfileTemplateEdge { node: APIKeyProfileTemplate cursor:
/// Cursor! }` — snapshot lines 1490-1498. `node` is nullable in the contract
/// (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfileTemplateEdge")]
pub struct APIKeyProfileTemplateEdge {
    pub node: Option<APIKeyProfileTemplate>,
    pub cursor: CursorScalar,
}

/// `type APIKeyProfileTemplateConnection` — snapshot lines 1473-1485.
/// `edges` is a nullable list of nullable edges (`[APIKeyProfileTemplateEdge]`),
/// exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfileTemplateConnection")]
pub struct APIKeyProfileTemplateConnection {
    pub edges: Option<Vec<Option<APIKeyProfileTemplateEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// `input APIKeyProfileTemplateOrder { direction: OrderDirection! = ASC field:
/// APIKeyProfileTemplateOrderField! }` — snapshot lines 1503-1512.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyProfileTemplateOrder")]
pub struct APIKeyProfileTemplateOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract
    /// (snapshot line 1507).
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: APIKeyProfileTemplateOrderField,
}

/// `input CreateAPIKeyProfileTemplateInput` — snapshot lines 2881-2891
/// (ent-generated). `name` is required, `description` is nullable (Go
/// `*string`, ent column stores ""), `projectID` is required.
///
/// Go parity (`biz/api_key_profile_template.go:34-57`): the service forces
/// `profile.Name = input.Name` (line 38-39) before persisting, so the input
/// profile's `name` field is overwritten by the template's `name` argument.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "CreateAPIKeyProfileTemplateInput")]
pub struct CreateAPIKeyProfileTemplateInput {
    pub name: String,
    pub description: Option<String>,
    #[graphql(name = "projectID")]
    pub project_id: ID,
}

/// `input UpdateAPIKeyProfileTemplateInput` — snapshot lines 7315-7324
/// (ent-generated). Both fields nullable (partial merge).
///
/// Go parity (`biz/api_key_profile_template.go:114-154`): when a profile is
/// supplied, the service sets `profile.Name = input.Name` if the name is
/// non-nil, otherwise reuses the existing template's name.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateAPIKeyProfileTemplateInput")]
pub struct UpdateAPIKeyProfileTemplateInput {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// `input LoadApiKeyProfileTemplateInput { templateID: ID! apiKeyID: ID! }` —
/// snapshot lines 409-412 / Go `conduit.graphql:404-407`. Custom (non-ent)
/// input for the `loadApiKeyProfileTemplate` mutation.
///
/// Go parity (`biz/api_key_profile_template.go:181-233`): clones the
/// template's profile, resolves a name-conflict-safe variant of the template
/// name (or profile name if non-empty), appends the profile to the target
/// APIKey's `profiles` list, and persists. The template and the APIKey MUST
/// belong to the same project.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "LoadApiKeyProfileTemplateInput")]
pub struct LoadApiKeyProfileTemplateInput {
    // All-caps acronym tags: pin every GraphQL name.
    #[graphql(name = "templateID")]
    pub template_id: ID,
    #[graphql(name = "apiKeyID")]
    pub api_key_id: ID,
}

/// `input APIKeyProfileTemplateWhereInput` — snapshot lines 1524-1605
/// (ent-generated predicate grammar). Implemented: `not`/`and`/`or`, every
/// scalar-field predicate family, and the `hasProject: Boolean` existence
/// predicate. The `hasProjectWith: [ProjectWhereInput!]` field references
/// the not-yet-ported `ProjectWhereInput` type and is therefore pending
/// (module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
#[graphql(name = "APIKeyProfileTemplateWhereInput")]
pub struct APIKeyProfileTemplateWhereInput {
    pub not: Option<Box<APIKeyProfileTemplateWhereInput>>,
    pub and: Option<Vec<APIKeyProfileTemplateWhereInput>>,
    pub or: Option<Vec<APIKeyProfileTemplateWhereInput>>,
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
    // description field predicates
    pub description: Option<String>,
    #[graphql(name = "descriptionNEQ")]
    pub description_neq: Option<String>,
    pub description_in: Option<Vec<String>>,
    pub description_not_in: Option<Vec<String>>,
    #[graphql(name = "descriptionGT")]
    pub description_gt: Option<String>,
    #[graphql(name = "descriptionGTE")]
    pub description_gte: Option<String>,
    #[graphql(name = "descriptionLT")]
    pub description_lt: Option<String>,
    #[graphql(name = "descriptionLTE")]
    pub description_lte: Option<String>,
    pub description_contains: Option<String>,
    pub description_has_prefix: Option<String>,
    pub description_has_suffix: Option<String>,
    pub description_equal_fold: Option<String>,
    pub description_contains_fold: Option<String>,
    // project_id field predicates (all-caps acronym: every name pinned)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // edge existence predicate; `hasProjectWith` pending — references
    // ProjectWhereInput (module doc).
    pub has_project: Option<bool>,
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:310 — CREATED_AT remaps to
// `ent.DefaultAPIKeyProfileTemplateOrder` = order by primary key,
// gql_pagination.go:413)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of the
/// GraphQL `APIKeyProfileTemplateOrderField` enum — it is ent's default order
/// (by primary key), which the Go resolver substitutes when the client asks
/// for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum APIKeyProfileTemplateOrderTerm {
    /// ent `DefaultAPIKeyProfileTemplateOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

/// The resolver-lowered ordering selection handed to
/// [`ProfileTemplateQueryServices::api_key_profile_templates_with_access`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct APIKeyProfileTemplateOrderSelection {
    pub direction: OrderDirection,
    pub term: APIKeyProfileTemplateOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.apiKeyProfileTemplates` (ent.resolvers.go:310): a
/// `CREATED_AT` request is remapped to the default ID order with the
/// requested direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_profile_template_order(
    order_by: Option<APIKeyProfileTemplateOrder>,
) -> Option<APIKeyProfileTemplateOrderSelection> {
    order_by.map(|order| APIKeyProfileTemplateOrderSelection {
        direction: order.direction,
        term: match order.field {
            APIKeyProfileTemplateOrderField::CreatedAt => APIKeyProfileTemplateOrderTerm::Id,
            APIKeyProfileTemplateOrderField::UpdatedAt => APIKeyProfileTemplateOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected). The host wires real repository /
// `biz.APIKeyProfileTemplateService` implementations; tests use the
// in-memory double defined under `tests`.
// ---------------------------------------------------------------------------

/// Error surface for the profile-template services. Messages mirror the Go
/// error strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("Template", name)`
///     (`internal/pkg/xerrors/graphql.go:104`: "%s name '%s' already exists")
///     — surfaced by `biz/api_key_profile_template.go:50,141`.
///   - cross-project load — `"template and API key must belong to the same
///     project"` (`biz/api_key_profile_template.go:197`).
///   - missing profile on load — `"template has no profile"`
///     (`biz/api_key_profile_template.go:202`).
///   - not found — wrapped ent `ent: apikeyprofiletemplate not found`.
///   - wrapped create/update/delete/query failures — the
///     `fmt.Errorf("failed to ...: %w")` prefixes in
///     `biz/api_key_profile_template.go`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProfileTemplateServiceError {
    #[error("profile template service is not available")]
    ServiceUnavailable,
    #[error("Template name '{0}' already exists")]
    DuplicateName(String),
    #[error("template and API key must belong to the same project")]
    CrossProjectLoad,
    #[error("template has no profile")]
    TemplateProfileMissing,
    #[error("ent: apikeyprofiletemplate not found")]
    NotFound,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("failed to create template: {0}")]
    Create(String),
    #[error("failed to update template: {0}")]
    Update(String),
    #[error("failed to delete template: {0}")]
    Delete(String),
    #[error("failed to load template: {0}")]
    Load(String),
    #[error("failed to query templates: {0}")]
    Query(String),
}

/// Arguments for the `apiKeyProfileTemplates` connection query, passed through
/// from the GraphQL layer verbatim (Go hands them straight to ent's
/// `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct APIKeyProfileTemplateConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<APIKeyProfileTemplateOrderSelection>,
    pub where_filter: Option<APIKeyProfileTemplateWhereInput>,
}

/// Backs `Query.apiKeyProfileTemplates` (Go `ent.resolvers.go:310`:
/// `r.client.APIKeyProfileTemplate.Query().Paginate(...)`).
#[async_trait::async_trait]
pub trait ProfileTemplateQueryServices: Send + Sync {
    async fn api_key_profile_templates_with_access(
        &self,
        access: &crate::policy::AdminAccessScope,
        args: APIKeyProfileTemplateConnectionArgs,
    ) -> Result<APIKeyProfileTemplateConnection, ProfileTemplateServiceError>;
}

/// Backs the four mutations (Go `biz.APIKeyProfileTemplateService`). `id` is
/// the raw GraphQL `ID!` scalar string; Go decodes it into `objects.GUID` and
/// passes `.ID` (int) to the service — concrete hosts perform the same
/// decode.
#[async_trait::async_trait]
pub trait ProfileTemplateMutationServices: Send + Sync {
    /// Mirrors `APIKeyProfileTemplateService.CreateTemplate`
    /// (`biz/api_key_profile_template.go:34`): force `profile.Name = input.Name`
    /// → create with `SetInput(input).SetProfile(profile)` → on
    /// `ent.IsConstraintError` surface the friendly duplicate-name error.
    async fn create_api_key_profile_template_with_access(
        &self,
        access: &crate::policy::AdminAccessScope,
        input: CreateAPIKeyProfileTemplateInput,
        profile: APIKeyProfileInput,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError>;

    /// Mirrors `APIKeyProfileTemplateService.UpdateTemplate`
    /// (`biz/api_key_profile_template.go:114`): RunInTransaction →
    /// SetInput(input); if profile supplied, reuse existing template name when
    /// input.Name is nil, force `profile.Name` accordingly, SetProfile; on
    /// constraint error surface duplicate-name.
    async fn update_api_key_profile_template_with_access(
        &self,
        access: &crate::policy::AdminAccessScope,
        id: &str,
        input: UpdateAPIKeyProfileTemplateInput,
        profile: Option<APIKeyProfileInput>,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError>;

    /// Mirrors `APIKeyProfileTemplateService.DeleteTemplate`
    /// (`biz/api_key_profile_template.go:156`): RunInTransaction → get then
    /// DeleteOneID → return the pre-delete snapshot.
    async fn delete_api_key_profile_template_with_access(
        &self,
        access: &crate::policy::AdminAccessScope,
        id: &str,
    ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError>;

    /// Mirrors `APIKeyProfileTemplateService.LoadTemplate`
    /// (`biz/api_key_profile_template.go:181`): RunInTransaction → get
    /// template + apiKey → cross-project guard → clone template.Profile →
    /// name-conflict-resolve → append to apiKey.Profiles → save.
    async fn load_api_key_profile_template_with_access(
        &self,
        access: &crate::policy::AdminAccessScope,
        input: LoadApiKeyProfileTemplateInput,
    ) -> Result<APIKey, ProfileTemplateServiceError>;
}

/// Resolves the injected [`ProfileTemplateQueryServices`] from the
/// async-graphql data bag; absent wiring surfaces the "service is not
/// available" failure mode (same convention as `apikey_query_services`).
pub(crate) fn profile_template_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ProfileTemplateQueryServices>, String> {
    match ctx.data::<Arc<dyn ProfileTemplateQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ProfileTemplateServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`ProfileTemplateMutationServices`] from the data
/// bag.
pub(crate) fn profile_template_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ProfileTemplateMutationServices>, String> {
    match ctx.data::<Arc<dyn ProfileTemplateMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ProfileTemplateServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::ID;

    use super::*;
    use crate::apikey::APIKeyStatus;
    use crate::pagination::connection_from_offset_page;
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go
    // `biz.APIKeyProfileTemplateService` call sequences without DB/HTTP; the
    // connection query mirrors the thin ent delegation (no predicate
    // lowering — `where` is recorded and passed through, as in Go where ent
    // lowers it).
    // ---------------------------------------------------------------------

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_template(id: i64, name: &str, project_id: &str) -> APIKeyProfileTemplate {
        APIKeyProfileTemplate {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            name: name.to_owned(),
            description: String::new(),
            project_id: ID::from(project_id),
            profile: Some(APIKeyProfile {
                name: name.to_owned(),
                ..Default::default()
            }),
        }
    }

    #[derive(Default, Clone)]
    struct InMemoryProfileTemplateService {
        templates: Arc<Mutex<Vec<APIKeyProfileTemplate>>>,
        api_keys: Arc<Mutex<Vec<APIKey>>>,
        captured_query_args: Arc<Mutex<Vec<APIKeyProfileTemplateConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl ProfileTemplateQueryServices for InMemoryProfileTemplateService {
        async fn api_key_profile_templates_with_access(
            &self,
            access: &crate::policy::AdminAccessScope,
            args: APIKeyProfileTemplateConnectionArgs,
        ) -> Result<APIKeyProfileTemplateConnection, ProfileTemplateServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<APIKeyProfileTemplate> = lock(&self.templates)
                .iter()
                .filter(|template| access.allows_project(template.project_id.as_str()))
                .cloned()
                .collect();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        APIKeyProfileTemplateOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        APIKeyProfileTemplateOrderTerm::UpdatedAt => {
                            a.updated_at.0.cmp(&b.updated_at.0)
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

    #[async_trait::async_trait]
    impl ProfileTemplateMutationServices for InMemoryProfileTemplateService {
        async fn create_api_key_profile_template_with_access(
            &self,
            access: &crate::policy::AdminAccessScope,
            input: CreateAPIKeyProfileTemplateInput,
            mut profile: APIKeyProfileInput,
        ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
            if !access.allows_project(input.project_id.as_str()) {
                return Err(ProfileTemplateServiceError::PermissionDenied(
                    "cannot create a template outside the authorized project".to_owned(),
                ));
            }
            let mut guard = lock(&self.templates);
            // biz/api_key_profile_template.go:48-52 — unique (project_id, name)
            // constraint surfaced as DuplicateNameError.
            if guard
                .iter()
                .any(|t| t.name == input.name && t.project_id == input.project_id)
            {
                return Err(ProfileTemplateServiceError::DuplicateName(input.name));
            }

            // biz/api_key_profile_template.go:38-39 — force profile.Name to the
            // template name.
            profile.name = input.name.clone();

            let id = guard.len() as i64 + 1;
            let created = APIKeyProfileTemplate {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                name: input.name,
                description: input.description.unwrap_or_default(),
                project_id: input.project_id,
                profile: Some(lower_profile_input(profile)),
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_api_key_profile_template_with_access(
            &self,
            access: &crate::policy::AdminAccessScope,
            id: &str,
            input: UpdateAPIKeyProfileTemplateInput,
            profile: Option<APIKeyProfileInput>,
        ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
            let mut guard = lock(&self.templates);
            let idx = guard
                .iter()
                .position(|t| t.id.as_str() == id && access.allows_project(t.project_id.as_str()))
                .ok_or_else(|| {
                    ProfileTemplateServiceError::Update(
                        ProfileTemplateServiceError::NotFound.to_string(),
                    )
                })?;

            let existing_name = guard[idx].name.clone();
            let existing_project = guard[idx].project_id.clone();

            // biz/api_key_profile_template.go:127-141 — duplicate-name probe on
            // rename (unique per project).
            if let Some(new_name) = &input.name
                && new_name != &existing_name
                && guard.iter().any(|t| {
                    t.name == *new_name && t.project_id == existing_project && t.id.as_str() != id
                })
            {
                return Err(ProfileTemplateServiceError::DuplicateName(new_name.clone()));
            }

            let template = &mut guard[idx];
            if let Some(name) = input.name {
                template.name = name;
            }
            if let Some(description) = input.description {
                template.description = description;
            }
            if let Some(mut profile_input) = profile {
                // biz/api_key_profile_template.go:127-131 — when input.Name is
                // nil, fall back to the existing template name.
                profile_input.name = template.name.clone();
                template.profile = Some(lower_profile_input(profile_input));
            }
            Ok(template.clone())
        }

        async fn delete_api_key_profile_template_with_access(
            &self,
            access: &crate::policy::AdminAccessScope,
            id: &str,
        ) -> Result<APIKeyProfileTemplate, ProfileTemplateServiceError> {
            let mut guard = lock(&self.templates);
            let idx = guard
                .iter()
                .position(|t| t.id.as_str() == id && access.allows_project(t.project_id.as_str()))
                .ok_or_else(|| {
                    ProfileTemplateServiceError::Delete(
                        ProfileTemplateServiceError::NotFound.to_string(),
                    )
                })?;
            Ok(guard.remove(idx))
        }

        async fn load_api_key_profile_template_with_access(
            &self,
            access: &crate::policy::AdminAccessScope,
            input: LoadApiKeyProfileTemplateInput,
        ) -> Result<APIKey, ProfileTemplateServiceError> {
            // biz/api_key_profile_template.go:181-233 — load + cross-project
            // guard + name-conflict resolve + append.
            let templates = lock(&self.templates).clone();
            let template = templates
                .iter()
                .find(|t| t.id == input.template_id && access.allows_project(t.project_id.as_str()))
                .ok_or_else(|| {
                    ProfileTemplateServiceError::Load(
                        ProfileTemplateServiceError::NotFound.to_string(),
                    )
                })?;

            let mut keys = lock(&self.api_keys);
            let api_key = keys
                .iter_mut()
                .find(|k| k.id == input.api_key_id && access.allows_project(k.project_id.as_str()))
                .ok_or_else(|| {
                    ProfileTemplateServiceError::Load("ent: apikey not found".to_string())
                })?;

            // biz/api_key_profile_template.go:196-198.
            if template.project_id != api_key.project_id {
                return Err(ProfileTemplateServiceError::CrossProjectLoad);
            }

            let template_profile = match template.profile.as_ref() {
                Some(p) => p.clone(),
                None => return Err(ProfileTemplateServiceError::TemplateProfileMissing),
            };

            let existing_profiles = api_key.profiles.clone().unwrap_or_default();
            let mut names: std::collections::HashSet<String> = existing_profiles
                .profiles
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.name)
                .collect();

            // biz/api_key_profile_template.go:210-214 — profile name (or
            // template name if empty) is the candidate.
            let mut candidate = template_profile.name.clone();
            if candidate.is_empty() {
                candidate = template.name.clone();
            }
            // biz/api_key_profile_template.go:235-251 — resolveProfileNameConflict.
            if names.contains(&candidate) {
                let mut i = 1;
                loop {
                    let probe = format!("{candidate} ({i})");
                    if !names.contains(&probe) {
                        candidate = probe;
                        break;
                    }
                    i += 1;
                }
            }

            let mut new_profile = template_profile.clone();
            new_profile.name = candidate.clone();
            names.insert(candidate);

            let mut combined = existing_profiles.profiles.unwrap_or_default();
            combined.push(new_profile);

            api_key.profiles = Some(crate::apikey::APIKeyProfiles {
                active_profile: existing_profiles.active_profile,
                profiles: Some(combined),
            });
            Ok(api_key.clone())
        }
    }

    /// Lowers the input shape to the OUTPUT `APIKeyProfile` GraphQL type,
    /// mirroring the Go `objects.APIKeyProfile` round-trip. Reuses the same
    /// reduction the apikey slice's `update_api_key_profiles` test double
    /// performs (`apikey.rs:1125-1161`).
    fn lower_profile_input(p: APIKeyProfileInput) -> APIKeyProfile {
        use crate::channel::ModelMapping;
        use crate::scalars::DecimalScalar;

        APIKeyProfile {
            name: p.name,
            model_mappings: p.model_mappings.map(|m| {
                m.into_iter()
                    .map(|mi| ModelMapping {
                        from: mi.from,
                        to: mi.to,
                    })
                    .collect()
            }),
            channel_ids: p.channel_ids,
            channel_tags: p.channel_tags,
            channel_tags_match_mode: p.channel_tags_match_mode,
            model_ids: p.model_ids,
            valid_from: p.valid_from,
            valid_until: p.valid_until,
            quota: p.quota.map(|q| crate::apikey::APIKeyQuota {
                requests: q.requests,
                total_tokens: q.total_tokens,
                cost: q.cost.map(|c| DecimalScalar(c.0)),
                period: crate::apikey::APIKeyQuotaPeriod {
                    period_type: q.period.period_type,
                    past_duration: q.period.past_duration.map(|pd| {
                        crate::apikey::APIKeyQuotaPastDuration {
                            value: pd.value,
                            unit: pd.unit,
                        }
                    }),
                    calendar_duration: q
                        .period
                        .calendar_duration
                        .map(|cd| crate::apikey::APIKeyQuotaCalendarDuration { unit: cd.unit }),
                },
            }),
            load_balance_strategy: p.load_balance_strategy,
            max_concurrent_requests: p.max_concurrent_requests,
        }
    }

    fn sample_api_key(id: i64, name: &str, project_id: &str) -> APIKey {
        APIKey {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            user_id: None,
            project_id: ID::from(project_id),
            key: format!("conduit-{id}"),
            name: name.to_owned(),
            key_type: crate::apikey::APIKeyType::User,
            status: APIKeyStatus::Enabled,
            scopes: None,
            profiles: None,
        }
    }

    fn schema_with(store: &InMemoryProfileTemplateService) -> AdminSchema {
        let query: Arc<dyn ProfileTemplateQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ProfileTemplateMutationServices> = Arc::new(store.clone());
        let mut request = conduit_auth::RequestContext::new();
        let _ = request.set_principal(conduit_auth::Principal::system());
        let _ = request.set_project_id("1");
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(request)
            .finish()
    }

    fn schema_with_project(
        store: &InMemoryProfileTemplateService,
        project_id: &str,
    ) -> AdminSchema {
        let query: Arc<dyn ProfileTemplateQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ProfileTemplateMutationServices> = Arc::new(store.clone());
        let principal = conduit_auth::Principal::user("profile-template-test-actor")
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::READ_API_KEYS,
            ))
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::WRITE_API_KEYS,
            ));
        let mut request = conduit_auth::RequestContext::new();
        let _ = request.set_principal(principal);
        let _ = request.set_project_id(project_id);
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(request)
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    fn assert_block_parity(
        sdl: &str,
        snapshot: &str,
        ours_header: &str,
        snapshot_header: &str,
        pending: &[&str],
    ) -> Result<(), TestError> {
        crate::sdl_parity::assert_block_parity(sdl, snapshot, ours_header, snapshot_header, pending)
    }

    // ---------------------------------------------------------------------
    // SDL parity: object types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_template_type_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type APIKeyProfileTemplate",
            "type APIKeyProfileTemplate",
            &[],
        )?;
        assert!(
            sdl.contains("type APIKeyProfileTemplate implements Node {"),
            "generated SDL must declare `type APIKeyProfileTemplate implements Node`"
        );
        assert!(
            snapshot.contains("type APIKeyProfileTemplate implements Node {"),
            "snapshot must declare `type APIKeyProfileTemplate implements Node`"
        );
        Ok(())
    }

    #[test]
    fn sdl_template_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type APIKeyProfileTemplateConnection",
            "type APIKeyProfileTemplateConnection",
            &[],
        )?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type APIKeyProfileTemplateEdge",
            "type APIKeyProfileTemplateEdge",
            &[],
        )
    }

    // ---------------------------------------------------------------------
    // SDL parity: input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_template_create_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input CreateAPIKeyProfileTemplateInput",
            "input CreateAPIKeyProfileTemplateInput",
            &[],
        )
    }

    #[test]
    fn sdl_template_update_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input UpdateAPIKeyProfileTemplateInput",
            "input UpdateAPIKeyProfileTemplateInput",
            &[],
        )
    }

    #[test]
    fn sdl_template_load_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input LoadApiKeyProfileTemplateInput",
            "input LoadApiKeyProfileTemplateInput",
            &[],
        )
    }

    #[test]
    fn sdl_template_order_matches_snapshot_with_asc_default() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input APIKeyProfileTemplateOrder",
            "input APIKeyProfileTemplateOrder",
            &[],
        )?;
        // Block comparison strips defaults; pin the ASC default exactly
        // (snapshot line 1507).
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "generated SDL must render the ASC default on APIKeyProfileTemplateOrder.direction"
        );
        assert!(snapshot.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    #[test]
    fn sdl_template_where_input_matches_snapshot_minus_pending_edge_filters()
    -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        // `hasProjectWith` references ProjectWhereInput (pending slice).
        assert_block_parity(
            &sdl,
            &snapshot,
            "input APIKeyProfileTemplateWhereInput",
            "input APIKeyProfileTemplateWhereInput",
            &["hasProjectWith: [ProjectWhereInput!]"],
        )
    }

    #[test]
    fn sdl_template_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "enum APIKeyProfileTemplateOrderField",
            "enum APIKeyProfileTemplateOrderField",
            &[],
        )
    }

    // ---------------------------------------------------------------------
    // SDL parity: root operation signatures
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_template_query_and_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // Query.apiKeyProfileTemplates — async-graphql renders arguments inline.
        assert!(
            sdl.contains(
                "apiKeyProfileTemplates(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: APIKeyProfileTemplateOrder, where: APIKeyProfileTemplateWhereInput): \
                 APIKeyProfileTemplateConnection!"
            ),
            "generated SDL missing the apiKeyProfileTemplates connection signature: {sdl}"
        );
        for token in [
            "after: Cursor",
            "first: Int",
            "before: Cursor",
            "last: Int",
            "orderBy: APIKeyProfileTemplateOrder",
            "where: APIKeyProfileTemplateWhereInput",
            "): APIKeyProfileTemplateConnection!",
        ] {
            assert!(
                snapshot.contains(token),
                "snapshot missing apiKeyProfileTemplates arg token `{token}`"
            );
        }

        // Mutations — async-graphql renders each signature inline on one line;
        // the snapshot (lines 871-883) splits each across multiple lines, so
        // we assert the SDL has the single-line form AND the snapshot carries
        // every argument token + the return type separately.
        // SDL single-line form:
        assert!(
            sdl.contains(
                "createApiKeyProfileTemplate(input: CreateAPIKeyProfileTemplateInput!, \
                 profile: APIKeyProfileInput!): APIKeyProfileTemplate!"
            ),
            "generated SDL missing createApiKeyProfileTemplate signature\n--- SDL tail ---\n{sdl}"
        );
        assert!(
            sdl.contains(
                "updateApiKeyProfileTemplate(id: ID!, input: \
                 UpdateAPIKeyProfileTemplateInput!, profile: APIKeyProfileInput): \
                 APIKeyProfileTemplate!"
            ),
            "generated SDL missing updateApiKeyProfileTemplate signature\n--- SDL tail ---\n{sdl}"
        );
        assert!(
            sdl.contains("deleteApiKeyProfileTemplate(id: ID!): APIKeyProfileTemplate!"),
            "generated SDL missing deleteApiKeyProfileTemplate signature\n--- SDL tail ---\n{sdl}"
        );
        assert!(
            sdl.contains(
                "loadApiKeyProfileTemplate(input: LoadApiKeyProfileTemplateInput!): APIKey!"
            ),
            "generated SDL missing loadApiKeyProfileTemplate signature\n--- SDL tail ---\n{sdl}"
        );

        // Snapshot multi-line form — verify every token is present.
        for token in [
            "createApiKeyProfileTemplate(",
            "input: CreateAPIKeyProfileTemplateInput!",
            "profile: APIKeyProfileInput!",
            "): APIKeyProfileTemplate!",
            "updateApiKeyProfileTemplate(",
            "input: UpdateAPIKeyProfileTemplateInput!",
            "profile: APIKeyProfileInput",
            "deleteApiKeyProfileTemplate(id: ID!): APIKeyProfileTemplate!",
            "loadApiKeyProfileTemplate(",
            "input: LoadApiKeyProfileTemplateInput!",
        ] {
            assert!(snapshot.contains(token), "snapshot missing token `{token}`");
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Ordering lowering (Go ent.resolvers.go:310)
    // ---------------------------------------------------------------------

    #[test]
    fn resolve_order_remaps_created_at_to_default_id_term() {
        let selection = resolve_profile_template_order(Some(APIKeyProfileTemplateOrder {
            direction: OrderDirection::Desc,
            field: APIKeyProfileTemplateOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(APIKeyProfileTemplateOrderSelection {
                direction: OrderDirection::Desc,
                term: APIKeyProfileTemplateOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_order_maps_updated_at_one_to_one() {
        let selection = resolve_profile_template_order(Some(APIKeyProfileTemplateOrder {
            direction: OrderDirection::Asc,
            field: APIKeyProfileTemplateOrderField::UpdatedAt,
        }));
        assert_eq!(
            selection.map(|s| s.term),
            Some(APIKeyProfileTemplateOrderTerm::UpdatedAt)
        );
        assert_eq!(resolve_profile_template_order(None), None);
    }

    // ---------------------------------------------------------------------
    // Resolver: createApiKeyProfileTemplate
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_template_forces_profile_name_to_template_name() -> Result<(), TestError> {
        // biz/api_key_profile_template.go:38-39: profile.Name is overwritten
        // with input.Name before persisting.
        let store = InMemoryProfileTemplateService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createApiKeyProfileTemplate(
                        input: { name: "tpl-1", description: "desc", projectID: "1" },
                        profile: { name: "ignored" }
                    ) {
                        id name description projectID
                        profile { name }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createApiKeyProfileTemplate"];
        assert_eq!(created["name"], "tpl-1");
        assert_eq!(created["description"], "desc");
        assert_eq!(created["projectID"], "1");
        // The input profile name "ignored" is overwritten to the template name.
        assert_eq!(created["profile"]["name"], "tpl-1");
        Ok(())
    }

    #[tokio::test]
    async fn create_template_rejects_duplicate_name_per_project() {
        // biz/api_key_profile_template.go:48-52: unique (project_id, name).
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(1, "dup", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createApiKeyProfileTemplate(
                        input: { name: "dup", projectID: "1" },
                        profile: { name: "x" }
                    ) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("Template name 'dup' already exists"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn create_template_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(
                r#"mutation {
                    createApiKeyProfileTemplate(
                        input: { name: "x", projectID: "1" },
                        profile: { name: "y" }
                    ) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("profile template service is not available"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateApiKeyProfileTemplate
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_template_renames_and_updates_description() -> Result<(), TestError> {
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(5, "old", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateApiKeyProfileTemplate(
                        id: "5",
                        input: { name: "new", description: "updated" }
                    ) { id name description }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updateApiKeyProfileTemplate"];
        assert_eq!(updated["name"], "new");
        assert_eq!(updated["description"], "updated");
        Ok(())
    }

    #[tokio::test]
    async fn update_template_rejects_duplicate_name_in_same_project() {
        // biz/api_key_profile_template.go:127-141.
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(1, "a", "1"));
        lock(&store.templates).push(sample_template(2, "b", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateApiKeyProfileTemplate(
                        id: "2",
                        input: { name: "a" }
                    ) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("Template name 'a' already exists"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn update_template_missing_id_surfaces_not_found() {
        let store = InMemoryProfileTemplateService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateApiKeyProfileTemplate(
                        id: "404",
                        input: { name: "x" }
                    ) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update template: ent: apikeyprofiletemplate not found"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: deleteApiKeyProfileTemplate
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn delete_template_returns_snapshot_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(3, "rm", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteApiKeyProfileTemplate(id: "3") { id name } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteApiKeyProfileTemplate"]["name"], "rm");
        assert!(lock(&store.templates).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn delete_template_missing_id_surfaces_not_found() {
        let store = InMemoryProfileTemplateService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteApiKeyProfileTemplate(id: "404") { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to delete template: ent: apikeyprofiletemplate not found"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: loadApiKeyProfileTemplate
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn load_template_appends_profile_to_api_key() -> Result<(), TestError> {
        // biz/api_key_profile_template.go:181-233 — clone template profile,
        // resolve name conflict, append.
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(7, "tpl", "1"));
        lock(&store.api_keys).push(sample_api_key(10, "k", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    loadApiKeyProfileTemplate(input: { templateID: "7", apiKeyID: "10" }) {
                        id
                        profiles { profiles { name } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let profiles = &data["loadApiKeyProfileTemplate"]["profiles"]["profiles"];
        assert_eq!(profiles.as_array().map(|a| a.len()), Some(1));
        assert_eq!(profiles[0]["name"], "tpl");
        Ok(())
    }

    #[tokio::test]
    async fn load_template_resolves_name_conflict_with_suffix() -> Result<(), TestError> {
        // biz/api_key_profile_template.go:235-251 — resolveProfileNameConflict
        // appends " (1)" when the candidate name already exists.
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(7, "tpl", "1"));
        let mut key = sample_api_key(10, "k", "1");
        key.profiles = Some(crate::apikey::APIKeyProfiles {
            active_profile: String::new(),
            profiles: Some(vec![APIKeyProfile {
                name: "tpl".to_owned(),
                ..Default::default()
            }]),
        });
        lock(&store.api_keys).push(key);
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    loadApiKeyProfileTemplate(input: { templateID: "7", apiKeyID: "10" }) {
                        profiles { profiles { name } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let profiles = &data["loadApiKeyProfileTemplate"]["profiles"]["profiles"];
        assert_eq!(profiles.as_array().map(|a| a.len()), Some(2));
        assert_eq!(profiles[1]["name"], "tpl (1)");
        Ok(())
    }

    #[tokio::test]
    async fn load_template_rejects_cross_project() {
        // biz/api_key_profile_template.go:196-198.
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(7, "tpl", "1"));
        lock(&store.api_keys).push(sample_api_key(10, "k", "2"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    loadApiKeyProfileTemplate(input: { templateID: "7", apiKeyID: "10" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("template and API key must belong to the same project"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: apiKeyProfileTemplates connection query
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn project_scope_hides_and_protects_foreign_templates_and_keys() -> Result<(), TestError>
    {
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).extend([
            sample_template(1, "project-a", "1"),
            sample_template(2, "project-b", "2"),
        ]);
        lock(&store.api_keys).extend([
            sample_api_key(10, "key-a", "1"),
            sample_api_key(20, "key-b", "2"),
        ]);
        let schema = schema_with_project(&store, "1");

        let query = schema
            .execute(
                r#"{
                    apiKeyProfileTemplates {
                        totalCount
                        edges { node { id name projectID } }
                    }
                }"#,
            )
            .await;
        assert!(query.errors.is_empty(), "errors: {:?}", query.errors);
        let query_data = query.data.into_json()?;
        assert_eq!(query_data["apiKeyProfileTemplates"]["totalCount"], 1);
        assert_eq!(
            query_data["apiKeyProfileTemplates"]["edges"][0]["node"]["name"],
            "project-a"
        );

        let create = schema
            .execute(
                r#"mutation {
                    createApiKeyProfileTemplate(
                        input: { name: "cross-create", projectID: "2" },
                        profile: { name: "ignored" }
                    ) { id }
                }"#,
            )
            .await;
        assert_eq!(create.errors.len(), 1);
        assert!(format!("{}", create.errors[0]).contains("permission denied"));

        let update = schema
            .execute(
                r#"mutation {
                    updateApiKeyProfileTemplate(id: "2", input: { name: "changed" }) { id }
                }"#,
            )
            .await;
        assert_eq!(update.errors.len(), 1);
        assert!(format!("{}", update.errors[0]).contains("ent: apikeyprofiletemplate not found"));

        let delete = schema
            .execute(r#"mutation { deleteApiKeyProfileTemplate(id: "2") { id } }"#)
            .await;
        assert_eq!(delete.errors.len(), 1);
        assert!(format!("{}", delete.errors[0]).contains("ent: apikeyprofiletemplate not found"));

        let load = schema
            .execute(
                r#"mutation {
                    loadApiKeyProfileTemplate(input: { templateID: "1", apiKeyID: "20" }) { id }
                }"#,
            )
            .await;
        assert_eq!(load.errors.len(), 1);
        assert!(format!("{}", load.errors[0]).contains("ent: apikey not found"));

        let templates = lock(&store.templates);
        assert_eq!(templates.len(), 2);
        assert_eq!(templates[1].name, "project-b");
        drop(templates);
        assert!(lock(&store.api_keys)[1].profiles.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn templates_query_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(1, "a", "1"));
        lock(&store.templates).push(sample_template(2, "b", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeyProfileTemplates {
                        totalCount
                        edges { cursor node { id name } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["apiKeyProfileTemplates"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        assert_eq!(connection["pageInfo"]["hasNextPage"], false);
        Ok(())
    }

    #[tokio::test]
    async fn templates_query_created_at_order_remaps_to_id_term() -> Result<(), TestError> {
        // Go Query.apiKeyProfileTemplates (ent.resolvers.go:310): CREATED_AT
        // is replaced by the default ID order with direction preserved.
        let store = InMemoryProfileTemplateService::default();
        lock(&store.templates).push(sample_template(1, "a", "1"));
        lock(&store.templates).push(sample_template(2, "b", "1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeyProfileTemplates(orderBy: { field: CREATED_AT, direction: DESC }) {
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
            Some(APIKeyProfileTemplateOrderSelection {
                direction: OrderDirection::Desc,
                term: APIKeyProfileTemplateOrderTerm::Id,
            })
        );
        let data = resp.data.into_json()?;
        assert_eq!(
            data["apiKeyProfileTemplates"]["edges"][0]["node"]["id"],
            "2"
        );
        Ok(())
    }

    #[tokio::test]
    async fn templates_query_passes_where_filter_through_to_service() -> Result<(), TestError> {
        let store = InMemoryProfileTemplateService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeyProfileTemplates(where: {
                        nameContainsFold: "prod",
                        projectIDIn: ["1", "2"]
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
            filter.project_id_in,
            Some(vec![ID::from("1"), ID::from("2")])
        );
        Ok(())
    }

    #[tokio::test]
    async fn templates_query_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(r#"{ apiKeyProfileTemplates { totalCount } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("profile template service is not available"),
            "unexpected error: {msg}"
        );
    }
}

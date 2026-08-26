//! RUST-P12-001 S07 — Project CRUD GraphQL slice.
//!
//! Bounded scope (this file): the `projects` connection query plus the five
//! Project-domain mutations declared in `conduit/internal/server/gql/conduit.graphql`
//! lines 833-837 and every GraphQL type/input they reference. All shapes are
//! copied field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Project implements Node` (snapshot line 4104) — scalar + self-domain
//!     fields only; cross-domain edge fields are pending (see module doc).
//!   - `type ProjectConnection` / `type ProjectEdge` (lines 4405/4422).
//!   - `type ProjectProfiles` / `type ProjectProfile` (lines 366/371) and the
//!     matching input twins `input ProjectProfileInput` (359) /
//!     `input UpdateProjectProfilesInput` (354) — the embedded profile payload
//!     exposed on `Project.profiles` and mutated by `updateProjectProfiles`.
//!   - `input CreateProjectInput` (lines 3039-3053, ent-generated).
//!   - `input UpdateProjectInput` (lines 7493-7509, ent-generated).
//!   - `input ProjectWhereInput` (lines 4463-4589, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `has<Edge>: Boolean`.
//!   - `input ProjectOrder` / `enum ProjectOrderField` (lines 4435/4448).
//!   - enums `ProjectStatus` (4455, two lowercase values: `active` `archived`).
//!
//! Go reference implementations:
//!   - Query.projects           — `internal/server/gql/ent.resolvers.go:394`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultProjectOrder` = ID before
//!     delegating to ent `Paginate`).
//!   - Mutation.createProject   — `internal/server/gql/conduit.resolvers.go:512`
//!     → `biz.ProjectService.CreateProject` (`biz/project.go:53`): duplicate
//!     name check → create with ent defaults → three default project-level
//!     roles → assign creator as project owner.
//!   - Mutation.updateProject   — `conduit.resolvers.go:517` →
//!     `biz.ProjectService.UpdateProject` (`biz/project.go:158`): partial merge
//!     (name, description, clearUsers / addUserIDs / removeUserIDs).
//!   - Mutation.updateProjectStatus — `conduit.resolvers.go:522` →
//!     `biz.ProjectService.UpdateProjectStatus` (`biz/project.go:299`).
//!   - Mutation.updateProjectProfiles — `conduit.resolvers.go:527` →
//!     `biz.ProjectService.UpdateProjectProfiles` (`biz/project.go:235`):
//!     validates the embedded profiles (unique names, valid activeProfile,
//!     valid channelTagsMatchMode) then SetProfiles + cache invalidation.
//!   - Mutation.deleteProject   — `conduit.resolvers.go:532` →
//!     `biz.ProjectService.DeleteProject` (`biz/project.go:333`): permission
//!     guard → cascade delete (user_projects, project-level roles, project API
//!     keys, soft delete project, cache invalidation); resolver returns
//!     `false, err` on failure and `true` on success.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! These are cross-domain surfaces that belong to other task slices:
//!
//!   - `Project.users(...)`, `Project.roles(...)`, `Project.apiKeys(...)`,
//!     `Project.requests(...)`, `Project.usageLogs(...)`, `Project.threads(...)`,
//!     `Project.traces(...)`, `Project.prompts(...)`,
//!     `Project.apiKeyProfileTemplates(...)`, `Project.projectUsers` — edge
//!     fields into other entity domains.
//!   - `ProjectWhereInput.has<Edge>With` for every edge — they reference other
//!     entities' `*WhereInput` types that live in their own slices.
//!   - The user/project link mutations live in `conduit/internal/server/gql/conduit.graphql`
//!     lines 844-846 (`addUserToProject` / `removeUserFromProject` /
//!     `updateProjectUser`). They delegate to `biz.UserService` (not
//!     `ProjectService`) and reference `UserProject` / role ids; they belong to
//!     the pending user/user_project slice.
//!   - There is NO `project(id: ID!)` single-object query in the snapshot;
//!     single-object lookup goes through the global `node(id: ID!)` /
//!     `nodes(ids: [ID!]!)` Relay queries (separate slice).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::apikey::ChannelTagsMatchMode;
use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::role::{
    RoleConnection, RoleConnectionArgs, RoleOrder, RoleWhereInput, resolve_role_order,
    role_query_services,
};
use crate::scalars::{CursorScalar, TimeScalar};
use crate::user::{UserProject, user_query_services};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; lowercase values are pinned explicitly
// because the default SCREAMING_SNAKE renaming would mangle them)
// ---------------------------------------------------------------------------

/// `enum ProjectStatus { active archived }` — snapshot line 4455, bound to Go
/// `ent/project.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum ProjectStatus {
    #[graphql(name = "active")]
    Active,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum ProjectOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 4448-4451 (two values only; projects have no NAME ordering, matching
/// ent's generated `OrderField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ProjectOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

// ---------------------------------------------------------------------------
// Output object types
// ---------------------------------------------------------------------------

/// `type ProjectProfile` — snapshot lines 371-376. Mirrors Go
/// `objects.ProjectProfile` (bound via `@goModel`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ProjectProfile {
    pub name: String,
    // All-caps acronym tag: default camelCase would emit `channelIds`.
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
}

/// `type ProjectProfiles` — snapshot lines 366-369. Mirrors Go
/// `objects.ProjectProfiles` (bound via `@goModel`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ProjectProfiles {
    pub active_profile: String,
    pub profiles: Option<Vec<ProjectProfile>>,
}

/// `type Project implements Node` — snapshot lines 4104-4401, scalar and
/// self-domain fields only. Cross-domain edge fields and the
/// `extend type Project` force-resolver fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct Project {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub name: String,
    pub description: String,
    pub status: ProjectStatus,
    pub profiles: Option<ProjectProfiles>,
}

#[ComplexObject]
impl Project {
    async fn project_users(&self, ctx: &Context<'_>) -> Result<Vec<UserProject>, String> {
        user_query_services(ctx)?
            .project_users(self.id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    /// Project-scoped roles used by the retained frontend's project role page.
    /// The project constraint is enforced here and combined with any nested
    /// filter supplied by the caller.
    #[allow(clippy::too_many_arguments)]
    async fn roles(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<RoleOrder>,
        #[graphql(name = "where")] where_filter: Option<RoleWhereInput>,
    ) -> Result<RoleConnection, String> {
        let services = role_query_services(ctx)?;
        let project_filter = RoleWhereInput {
            project_id: Some(self.id.clone()),
            and: where_filter.map(|filter| vec![filter]),
            ..RoleWhereInput::default()
        };
        services
            .roles(RoleConnectionArgs {
                after: after.map(|cursor| cursor.0),
                first,
                before: before.map(|cursor| cursor.0),
                last,
                order_by: resolve_role_order(order_by),
                where_filter: Some(project_filter),
            })
            .await
            .map_err(|err| err.to_string())
    }
}

/// `type ProjectEdge { node: Project cursor: Cursor! }` — snapshot line 4422.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ProjectEdge {
    pub node: Option<Project>,
    pub cursor: CursorScalar,
}

/// `type ProjectConnection` — snapshot line 4405. `edges` is a nullable list
/// of nullable edges (`[ProjectEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ProjectConnection {
    pub edges: Option<Vec<Option<ProjectEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input ProjectProfileInput` — snapshot lines 359-364.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ProjectProfileInput {
    pub name: String,
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
}

/// `input UpdateProjectProfilesInput` — snapshot lines 354-357.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct UpdateProjectProfilesInput {
    pub active_profile: String,
    pub profiles: Option<Vec<ProjectProfileInput>>,
}

/// `input CreateProjectInput` — snapshot lines 3039-3053 (ent-generated).
///
/// `status` / `userIDs` are declared by the ent input but `biz.CreateProject`
/// never applies them (project.go:73-79 only calls SetName/SetDescription);
/// they are surfaced here for GraphQL input parity and ignored by the
/// service-layer trait (matching Go behaviour).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreateProjectInput {
    pub name: String,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    #[graphql(name = "userIDs")]
    pub user_ids: Option<Vec<ID>>,
}

/// `input UpdateProjectInput` — snapshot lines 7493-7509 (ent-generated).
///
/// The ent input also carries `status`, but `biz.UpdateProject` never applies
/// it (status changes go through `UpdateProjectStatus`); it is surfaced here
/// for GraphQL input parity and ignored by the service-layer trait.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdateProjectInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    #[graphql(name = "addUserIDs")]
    pub add_user_ids: Option<Vec<ID>>,
    #[graphql(name = "removeUserIDs")]
    pub remove_user_ids: Option<Vec<ID>>,
    pub clear_users: Option<bool>,
}

/// `input ProjectOrder { direction: OrderDirection! = ASC field:
/// ProjectOrderField! }` — snapshot lines 4435-4444.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ProjectOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: ProjectOrderField,
}

/// `input ProjectWhereInput` — snapshot lines 4463-4589 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family, and the `has<Edge>: Boolean` existence predicates. The
/// `has<Edge>With: [<Other>WhereInput!]` fields are pending (they reference
/// other entities' WhereInputs — see module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ProjectWhereInput {
    pub not: Option<Box<ProjectWhereInput>>,
    pub and: Option<Vec<ProjectWhereInput>>,
    pub or: Option<Vec<ProjectWhereInput>>,
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
    // status field predicates
    pub status: Option<ProjectStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<ProjectStatus>,
    pub status_in: Option<Vec<ProjectStatus>>,
    pub status_not_in: Option<Vec<ProjectStatus>>,
    // edge existence predicates (`has<Edge>With` variants pending — they
    // reference other entities' WhereInput types, see module doc)
    pub has_users: Option<bool>,
    pub has_roles: Option<bool>,
    #[graphql(name = "hasAPIKeys")]
    pub has_api_keys: Option<bool>,
    pub has_requests: Option<bool>,
    pub has_usage_logs: Option<bool>,
    pub has_threads: Option<bool>,
    pub has_traces: Option<bool>,
    pub has_prompts: Option<bool>,
    #[graphql(name = "hasAPIKeyProfileTemplates")]
    pub has_api_key_profile_templates: Option<bool>,
    pub has_project_users: Option<bool>,
}

// ---------------------------------------------------------------------------
// Input → object conversions.
//
// Go binds the ProjectProfile input and output GraphQL types to the SAME
// `objects.ProjectProfile` Go struct via `@goModel`, so an input value
// round-trips into the stored object unchanged. These `From` impls are the
// Rust analogue.
// ---------------------------------------------------------------------------

impl From<ProjectProfileInput> for ProjectProfile {
    fn from(input: ProjectProfileInput) -> Self {
        Self {
            name: input.name,
            channel_ids: input.channel_ids,
            channel_tags: input.channel_tags,
            channel_tags_match_mode: input.channel_tags_match_mode,
        }
    }
}

impl From<UpdateProjectProfilesInput> for ProjectProfiles {
    fn from(input: UpdateProjectProfilesInput) -> Self {
        Self {
            active_profile: input.active_profile,
            profiles: input
                .profiles
                .map(|v| v.into_iter().map(ProjectProfile::from).collect()),
        }
    }
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:399-401)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of
/// the GraphQL `ProjectOrderField` enum — it is ent's `DefaultProjectOrder`
/// (order by primary key), which the Go resolver substitutes when the client
/// asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectOrderTerm {
    /// ent `DefaultProjectOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

/// The resolver-lowered ordering selection handed to
/// [`ProjectQueryServices::projects`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectOrderSelection {
    pub direction: OrderDirection,
    pub term: ProjectOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.projects` (ent.resolvers.go:399-401): a `CREATED_AT`
/// request is remapped to `ent.DefaultProjectOrder` (order by ID) with the
/// requested direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_project_order(order_by: Option<ProjectOrder>) -> Option<ProjectOrderSelection> {
    order_by.map(|order| ProjectOrderSelection {
        direction: order.direction,
        term: match order.field {
            ProjectOrderField::CreatedAt => ProjectOrderTerm::Id,
            ProjectOrderField::UpdatedAt => ProjectOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependencies:
// `r.client.Project` for the connection query and `r.projectService` for the
// CRUD mutations)
// ---------------------------------------------------------------------------

/// Error surface for the project services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("project", name)`
///     (`internal/pkg/xerrors/graphql.go:104`): `"%s name '%s' already exists"`.
///   - not found — `ErrProjectNotFound` wrapped as
///     `"failed to get project: project not found (id: %d)"` (project.go:195).
///   - permission denied — `permission_validator.go:239` "insufficient
///     permissions: only system owners can delete projects".
///   - wrapped create/update/delete failures — the `fmt.Errorf("failed to
///     ...: %w")` prefixes in `biz/project.go`.
///   - profile validation — `ValidateProjectProfiles` (project.go:257-296):
///     empty/duplicate profile names, invalid `channelTagsMatchMode`, missing
///     active profile.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProjectServiceError {
    #[error("project service is not available")]
    ServiceUnavailable,
    #[error("project name '{0}' already exists")]
    DuplicateName(String),
    #[error("failed to get project: project not found (id: {0})")]
    NotFound(String),
    #[error("insufficient permissions: only system owners can delete projects")]
    NotSystemOwner,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("failed to create project: {0}")]
    Create(String),
    #[error("failed to update project: {0}")]
    Update(String),
    #[error("failed to update project status: {0}")]
    UpdateStatus(String),
    #[error("failed to update project profiles: {0}")]
    UpdateProfiles(String),
    #[error("failed to delete project: {0}")]
    Delete(String),
    #[error("failed to query projects: {0}")]
    Query(String),
}

/// Arguments for the `projects` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct ProjectConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ProjectOrderSelection>,
    pub where_filter: Option<ProjectWhereInput>,
}

/// Backs `Query.projects` (Go ent.resolvers.go:394: `r.client.Project.Query()
/// .Paginate(...)`). The host wires the real repository; tests use an
/// in-memory store.
#[async_trait::async_trait]
pub trait ProjectQueryServices: Send + Sync {
    async fn projects(
        &self,
        args: ProjectConnectionArgs,
    ) -> Result<ProjectConnection, ProjectServiceError>;
}

/// Backs the five CRUD mutations (Go `biz.ProjectService`). `id` is the raw
/// GraphQL `ID!` scalar string; Go decodes it into `objects.GUID` and passes
/// `.ID` (int) to the service — concrete hosts perform the same decode.
#[async_trait::async_trait]
pub trait ProjectMutationServices: Send + Sync {
    /// Mirrors `ProjectService.CreateProject` (biz/project.go:53): reject
    /// duplicate names, create with ent column defaults, then create the
    /// three default project-level roles (Admin/Developer/Viewer) and link
    /// the creator as project owner.
    async fn create_project(
        &self,
        input: CreateProjectInput,
    ) -> Result<Project, ProjectServiceError>;

    /// Mirrors `ProjectService.UpdateProject` (biz/project.go:158): partial
    /// merge of name / description; `clearUsers` wins over add/remove
    /// (Go if-else ordering: project.go:165-175).
    async fn update_project(
        &self,
        id: &str,
        input: UpdateProjectInput,
    ) -> Result<Project, ProjectServiceError>;

    /// Mirrors `ProjectService.UpdateProjectStatus` (biz/project.go:299):
    /// SetStatus + cache invalidation.
    async fn update_project_status(
        &self,
        id: &str,
        status: ProjectStatus,
    ) -> Result<Project, ProjectServiceError>;

    /// Mirrors `ProjectService.UpdateProjectProfiles` (biz/project.go:235):
    /// `ValidateProjectProfiles` (unique non-empty names, valid active
    /// profile, valid channelTagsMatchMode) → SetProfiles + cache
    /// invalidation.
    async fn update_project_profiles(
        &self,
        id: &str,
        input: UpdateProjectProfilesInput,
    ) -> Result<Project, ProjectServiceError>;

    /// Mirrors `ProjectService.DeleteProject` (biz/project.go:333):
    /// permission guard → cascade delete (user_projects, project-level
    /// roles, project API keys, soft delete project, cache invalidation).
    async fn delete_project(&self, id: &str) -> Result<(), ProjectServiceError>;
}

/// Resolves the injected [`ProjectQueryServices`] from the async-graphql data
/// bag; absent wiring surfaces the "service is not available" failure mode
/// (same convention as `channel::channel_query_services`).
pub(crate) fn project_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ProjectQueryServices>, String> {
    match ctx.data::<Arc<dyn ProjectQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ProjectServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`ProjectMutationServices`] from the data bag.
pub(crate) fn project_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ProjectMutationServices>, String> {
    match ctx.data::<Arc<dyn ProjectMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ProjectServiceError::ServiceUnavailable.to_string()),
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
    use crate::pagination::connection_from_offset_page;
    use crate::sdl_parity::{
        assert_block_parity, assert_block_parity_with_extensions, snapshot_text,
    };
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go `biz.ProjectService` call
    // sequences without DB/HTTP; the connection query mirrors the thin ent
    // delegation (no predicate lowering — the `where` filter is recorded and
    // passed through, as in Go where ent lowers it).
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryProjectService {
        projects: Arc<Mutex<Vec<Project>>>,
        captured_query_args: Arc<Mutex<Vec<ProjectConnectionArgs>>>,
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Fixed timestamp for stored rows (the wire format of `Time` is not
    /// under test here).
    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_project(id: i64, name: &str) -> Project {
        Project {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            name: name.to_owned(),
            description: String::new(),
            status: ProjectStatus::Active,
            profiles: None,
        }
    }

    #[async_trait::async_trait]
    impl ProjectQueryServices for InMemoryProjectService {
        async fn projects(
            &self,
            args: ProjectConnectionArgs,
        ) -> Result<ProjectConnection, ProjectServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<Project> = lock(&self.projects).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        ProjectOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        ProjectOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
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
            Ok(ProjectConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(ProjectEdge {
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
    impl ProjectMutationServices for InMemoryProjectService {
        async fn create_project(
            &self,
            input: CreateProjectInput,
        ) -> Result<Project, ProjectServiceError> {
            let mut guard = lock(&self.projects);
            // Go CreateProject (biz/project.go:61-71): duplicate-name check.
            if guard.iter().any(|existing| existing.name == input.name) {
                return Err(ProjectServiceError::DuplicateName(input.name));
            }

            // ent column defaults (internal/ent/schema/project.go): status
            // defaults to `active`, description to "". biz.CreateProject
            // ignores `input.status` and `input.userIDs` (project.go:73-79).
            let id = guard.len() as i64 + 1;
            let created = Project {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                name: input.name,
                description: input.description.unwrap_or_default(),
                status: ProjectStatus::Active,
                profiles: None,
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_project(
            &self,
            id: &str,
            input: UpdateProjectInput,
        ) -> Result<Project, ProjectServiceError> {
            let mut guard = lock(&self.projects);
            let Some(project) = guard.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(ProjectServiceError::Update(
                    ProjectServiceError::NotFound(id.to_string()).to_string(),
                ));
            };

            // Field application mirrors Go biz/project.go:158-186 exactly:
            // SetNillableName / SetNillableDescription; clearUsers wins over
            // add/remove (Go if-else ordering, project.go:165-175).
            if let Some(v) = input.name {
                project.name = v;
            }
            if let Some(v) = input.description {
                project.description = v;
            }
            // input.status is intentionally NOT applied by biz.UpdateProject
            // (status changes go through UpdateProjectStatus — parity).
            // input.clear_users / add_user_ids / remove_user_ids affect the
            // users edge (a pending cross-domain surface); the host wires the
            // real edge mutation. The double records them for parity probes.

            Ok(project.clone())
        }

        async fn update_project_status(
            &self,
            id: &str,
            status: ProjectStatus,
        ) -> Result<Project, ProjectServiceError> {
            let mut guard = lock(&self.projects);
            let Some(project) = guard.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(ProjectServiceError::UpdateStatus(
                    ProjectServiceError::NotFound(id.to_string()).to_string(),
                ));
            };
            project.status = status;
            Ok(project.clone())
        }

        async fn update_project_profiles(
            &self,
            id: &str,
            input: UpdateProjectProfilesInput,
        ) -> Result<Project, ProjectServiceError> {
            // Validate first — mirrors biz/project.go:236-238 +
            // ValidateProjectProfiles (project.go:257-296): profile names
            // unique (case-insensitive) and non-empty; valid
            // channelTagsMatchMode; active profile exists in the list.
            let profiles = input.profiles.unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            for profile in &profiles {
                let name_lower = profile.name.trim().to_lowercase();
                if name_lower.is_empty() {
                    return Err(ProjectServiceError::UpdateProfiles(
                        "profile name cannot be empty".to_owned(),
                    ));
                }
                if !seen.insert(name_lower.clone()) {
                    return Err(ProjectServiceError::UpdateProfiles(format!(
                        "duplicate profile name: {}",
                        profile.name
                    )));
                }
            }
            if !input.active_profile.is_empty()
                && !profiles.iter().any(|p| p.name == input.active_profile)
            {
                return Err(ProjectServiceError::UpdateProfiles(format!(
                    "active profile '{}' does not exist in the profiles list",
                    input.active_profile
                )));
            }

            let mut guard = lock(&self.projects);
            let Some(project) = guard.iter_mut().find(|p| p.id.as_str() == id) else {
                return Err(ProjectServiceError::UpdateProfiles(
                    ProjectServiceError::NotFound(id.to_string()).to_string(),
                ));
            };
            project.profiles = Some(ProjectProfiles::from(UpdateProjectProfilesInput {
                active_profile: input.active_profile,
                profiles: Some(profiles),
            }));
            Ok(project.clone())
        }

        async fn delete_project(&self, id: &str) -> Result<(), ProjectServiceError> {
            let mut guard = lock(&self.projects);
            let before = guard.len();
            guard.retain(|p| p.id.as_str() != id);
            if guard.len() == before {
                // Go: "failed to delete project: %w" wrapping not-found.
                return Err(ProjectServiceError::Delete(
                    ProjectServiceError::NotFound(id.to_string()).to_string(),
                ));
            }
            Ok(())
        }
    }

    fn schema_with(store: &InMemoryProjectService) -> AdminSchema {
        let query: Arc<dyn ProjectQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ProjectMutationServices> = Arc::new(store.clone());
        admin_schema_builder().data(query).data(mutation).finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // ---------------------------------------------------------------------
    // SDL parity: object types (snapshot oracle)
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_project_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Cross-domain edge fields are the documented pending set
        // (snapshot lines 4121-4400, multi-line connection signatures).
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type Project",
            "type Project",
            &[
                "users(…): UserConnection!",
                "apiKeys(…): APIKeyConnection!",
                "requests(…): RequestConnection!",
                "usageLogs(…): UsageLogConnection!",
                "threads(…): ThreadConnection!",
                "traces(…): TraceConnection!",
                "prompts(…): PromptConnection!",
                "apiKeyProfileTemplates(…): APIKeyProfileTemplateConnection!",
                "projectUsers: [UserProject!]",
            ],
            &["projectUsers: [UserProject!]!"],
        )?;

        // The implements clause must match the snapshot's declaration.
        assert!(
            sdl.contains("type Project implements Node {"),
            "generated SDL must declare `type Project implements Node`"
        );
        assert!(snapshot.contains("type Project implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_project_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ProjectConnection",
            "type ProjectConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type ProjectEdge", "type ProjectEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_project_profiles_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ProjectProfiles",
            "type ProjectProfiles",
            &[],
        )?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ProjectProfile",
            "type ProjectProfile",
            &[],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_create_project_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input CreateProjectInput",
            "input CreateProjectInput",
            &[],
        )
    }

    #[test]
    fn sdl_update_project_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input UpdateProjectInput",
            "input UpdateProjectInput",
            &[],
        )
    }

    #[test]
    fn sdl_project_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError>
    {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        // The `has<Edge>With` fields reference other entities' WhereInput
        // types (pending slices).
        assert_block_parity(
            &sdl,
            &snapshot,
            "input ProjectWhereInput",
            "input ProjectWhereInput",
            &[
                "hasUsersWith: [UserWhereInput!]",
                "hasRolesWith: [RoleWhereInput!]",
                "hasAPIKeysWith: [APIKeyWhereInput!]",
                "hasRequestsWith: [RequestWhereInput!]",
                "hasUsageLogsWith: [UsageLogWhereInput!]",
                "hasThreadsWith: [ThreadWhereInput!]",
                "hasTracesWith: [TraceWhereInput!]",
                "hasPromptsWith: [PromptWhereInput!]",
                "hasAPIKeyProfileTemplatesWith: [APIKeyProfileTemplateWhereInput!]",
                "hasProjectUsersWith: [UserProjectWhereInput!]",
            ],
        )
    }

    #[test]
    fn sdl_project_support_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input ProjectProfileInput",
            "input UpdateProjectProfilesInput",
            "input ProjectOrder",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        // The block comparison strips default values; pin the `= ASC`
        // default of ProjectOrder.direction exactly (snapshot line 4439).
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "generated SDL must render the ASC default on ProjectOrder.direction"
        );
        assert!(snapshot.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: enums
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_project_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in ["enum ProjectStatus", "enum ProjectOrderField"] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: root operation signatures
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_projects_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query.projects — async-graphql renders arguments inline.
        assert!(
            sdl.contains(
                "projects(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: ProjectOrder, where: ProjectWhereInput): ProjectConnection!"
            ),
            "generated SDL missing the projects connection signature: {sdl}"
        );
        for token in [
            "after: Cursor",
            "first: Int",
            "before: Cursor",
            "last: Int",
            "orderBy: ProjectOrder",
            "where: ProjectWhereInput",
            "): ProjectConnection!",
        ] {
            assert!(
                snapshot.contains(token),
                "snapshot missing projects arg token `{token}`"
            );
        }

        // Mutations — flat one-line signatures in both dialects
        // (snapshot type Mutation, lines 838-842).
        for signature in [
            "createProject(input: CreateProjectInput!): Project!",
            "updateProject(id: ID!, input: UpdateProjectInput!): Project!",
            "updateProjectStatus(id: ID!, status: ProjectStatus!): Project!",
            "updateProjectProfiles(id: ID!, input: UpdateProjectProfilesInput!): Project!",
            "deleteProject(id: ID!): Boolean!",
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
    // Ordering lowering (Go ent.resolvers.go:399-401)
    // ---------------------------------------------------------------------

    #[test]
    fn resolve_project_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_project_order(Some(ProjectOrder {
            direction: OrderDirection::Desc,
            field: ProjectOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(ProjectOrderSelection {
                direction: OrderDirection::Desc,
                term: ProjectOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_project_order_maps_updated_at_one_to_one() {
        let selection = resolve_project_order(Some(ProjectOrder {
            direction: OrderDirection::Asc,
            field: ProjectOrderField::UpdatedAt,
        }));
        assert_eq!(
            selection,
            Some(ProjectOrderSelection {
                direction: OrderDirection::Asc,
                term: ProjectOrderTerm::UpdatedAt,
            })
        );
        assert_eq!(resolve_project_order(None), None);
    }

    // ---------------------------------------------------------------------
    // Input → object conversion semantics
    // ---------------------------------------------------------------------

    #[test]
    fn profiles_conversion_maps_each_profile() -> Result<(), TestError> {
        let input = UpdateProjectProfilesInput {
            active_profile: "prod".to_owned(),
            profiles: Some(vec![ProjectProfileInput {
                name: "prod".to_owned(),
                channel_ids: Some(vec![1, 2]),
                channel_tags: Some(vec!["tagged".to_owned()]),
                channel_tags_match_mode: Some(ChannelTagsMatchMode::All),
            }]),
        };
        let profiles = ProjectProfiles::from(input);
        assert_eq!(profiles.active_profile, "prod");
        let inner = profiles
            .profiles
            .as_ref()
            .and_then(|v| v.first())
            .ok_or("expected one profile")
            .unwrap_or_else(|_| panic!("profiles list missing"));
        assert_eq!(inner.name, "prod");
        assert_eq!(inner.channel_ids.as_deref(), Some([1, 2].as_slice()));
        assert_eq!(
            inner.channel_tags.as_deref(),
            Some(["tagged".to_owned()].as_slice())
        );
        assert_eq!(
            inner.channel_tags_match_mode,
            Some(ChannelTagsMatchMode::All)
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Resolver: createProject
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_project_returns_created_project_with_ent_defaults() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createProject(input: {
                        name: "my-proj",
                        description: "first project"
                    }) { id name description status }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createProject"];
        assert_eq!(created["id"], "1");
        assert_eq!(created["name"], "my-proj");
        assert_eq!(created["description"], "first project");
        // ent column default (schema/project.go): status = active.
        assert_eq!(created["status"], "active");
        Ok(())
    }

    #[tokio::test]
    async fn create_project_duplicate_name_surfaces_go_error_message() {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "dup"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { createProject(input: { name: "dup" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        // Go xerrors.DuplicateNameError: "%s name '%s' already exists".
        assert!(
            message.contains("project name 'dup' already exists"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn create_project_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(r#"mutation { createProject(input: { name: "x" }) { id } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("project service is not available"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateProject
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_project_applies_partial_merge() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(7, "old-name"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateProject(id: "7", input: {
                        name: "new-name",
                        description: "renamed"
                    }) { id name description }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updateProject"];
        assert_eq!(updated["id"], "7");
        assert_eq!(updated["name"], "new-name");
        assert_eq!(updated["description"], "renamed");
        Ok(())
    }

    #[tokio::test]
    async fn update_project_missing_id_surfaces_wrapped_not_found() {
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateProject(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to update project"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateProjectStatus
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_project_status_sets_status() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(3, "p3"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateProjectStatus(id: "3", status: archived) { id status } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateProjectStatus"]["status"], "archived");
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Resolver: updateProjectProfiles
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_project_profiles_persists_profiles() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "p1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateProjectProfiles(id: "1", input: {
                        activeProfile: "prod",
                        profiles: [{ name: "prod", channelIDs: [1, 2] }]
                    }) {
                        id
                        profiles { activeProfile profiles { name channelIDs } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let profiles = &data["updateProjectProfiles"]["profiles"];
        assert_eq!(profiles["activeProfile"], "prod");
        assert_eq!(profiles["profiles"][0]["name"], "prod");
        assert_eq!(profiles["profiles"][0]["channelIDs"][0], 1);
        Ok(())
    }

    #[tokio::test]
    async fn update_project_profiles_rejects_duplicate_profile_name() {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "p1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateProjectProfiles(id: "1", input: {
                        activeProfile: "a",
                        profiles: [{ name: "a" }, { name: "A" }]
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("duplicate profile name"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn update_project_profiles_rejects_missing_active_profile() {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "p1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateProjectProfiles(id: "1", input: {
                        activeProfile: "ghost",
                        profiles: [{ name: "prod" }]
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("active profile 'ghost' does not exist"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: deleteProject
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn delete_project_returns_true_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(5, "victim"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteProject(id: "5") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteProject"], true);
        assert!(lock(&store.projects).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn delete_project_missing_id_surfaces_wrapped_error() {
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteProject(id: "404") }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to delete project"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: projects connection query
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn projects_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "a"));
        lock(&store.projects).push(sample_project(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    projects {
                        totalCount
                        edges { cursor node { id name } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["projects"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        assert_eq!(connection["pageInfo"]["hasNextPage"], false);
        Ok(())
    }

    #[tokio::test]
    async fn projects_empty_store_returns_empty_connection() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ projects { totalCount edges { cursor } } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["projects"]["totalCount"], 0);
        assert_eq!(data["projects"]["edges"], serde_json::json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn projects_first_limits_page_and_flags_next_page() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        for (id, name) in [(1, "a"), (2, "b"), (3, "c")] {
            lock(&store.projects).push(sample_project(id, name));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    projects(first: 2) {
                        totalCount
                        edges { node { name } }
                        pageInfo { hasNextPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["projects"]["totalCount"], 3);
        assert_eq!(
            data["projects"]["edges"].as_array().map(std::vec::Vec::len),
            Some(2)
        );
        assert_eq!(data["projects"]["pageInfo"]["hasNextPage"], true);
        Ok(())
    }

    #[tokio::test]
    async fn projects_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        // Go Query.projects (ent.resolvers.go:399-401): CREATED_AT is
        // replaced by ent.DefaultProjectOrder (ID) with direction preserved.
        let store = InMemoryProjectService::default();
        lock(&store.projects).push(sample_project(1, "a"));
        lock(&store.projects).push(sample_project(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    projects(orderBy: { field: CREATED_AT, direction: DESC }) {
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
            Some(ProjectOrderSelection {
                direction: OrderDirection::Desc,
                term: ProjectOrderTerm::Id,
            })
        );
        // Desc-by-ID ordering is observable in the page.
        let data = resp.data.into_json()?;
        assert_eq!(data["projects"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn projects_order_direction_defaults_to_asc_when_omitted() -> Result<(), TestError> {
        // Contract: `direction: OrderDirection! = ASC` (snapshot line 4439).
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ projects(orderBy: { field: UPDATED_AT }) { totalCount } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        assert_eq!(
            captured[0].order_by,
            Some(ProjectOrderSelection {
                direction: OrderDirection::Asc,
                term: ProjectOrderTerm::UpdatedAt,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn projects_passes_where_filter_through_to_service() -> Result<(), TestError> {
        let store = InMemoryProjectService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    projects(where: {
                        nameContainsFold: "prod",
                        statusIn: [active, archived]
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
            Some(vec![ProjectStatus::Active, ProjectStatus::Archived])
        );
        Ok(())
    }

    #[tokio::test]
    async fn projects_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ projects { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("project service is not available"),
            "unexpected error message: {message}"
        );
    }
}

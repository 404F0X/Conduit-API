//! RUST-P12-001 S07 (Pauli-13th) — Role-domain GraphQL slice.
//!
//! Bounded scope: the `roles` connection query plus the four Role-domain
//! mutations declared in `conduit/internal/server/gql/conduit.graphql`
//! lines 828-831 and every GraphQL type/input they reference. All shapes
//! are copied field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Role implements Node` (snapshot line 6607) — scalar/self-domain
//!     fields only; cross-domain edge fields (`users(…)`, `project`,
//!     `userRoles`) are pending (see module doc below).
//!   - `type RoleConnection` / `type RoleEdge` (lines 6661 / 6678).
//!   - `enum RoleLevel` (line 6689, two lowercase values: `system` `project`).
//!   - `input CreateRoleInput` (lines 3163-3177, ent-generated).
//!   - `input UpdateRoleInput` (lines 7622-7636, ent-generated).
//!   - `input RoleOrder` / `enum RoleOrderField` (lines 6698 / 6711).
//!   - `input RoleWhereInput` (lines 6719-6793, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `has<Edge>: Boolean`.
//!
//! Go reference implementations:
//!   - Query.roles             — `internal/server/gql/ent.resolvers.go:458`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultRoleOrder` = ID before
//!     delegating to ent `Paginate`).
//!   - Mutation.createRole     — `internal/server/gql/conduit.resolvers.go:480`
//!     → `biz.RoleService.CreateRole` (`biz/role.go:42`): scope-permission
//!     check, level/projectID consistency check, duplicate-name probe, ent
//!     create with name+scopes+level+projectID.
//!   - Mutation.updateRole     — `conduit.resolvers.go:485` →
//!     `biz.RoleService.UpdateRole` (`biz/role.go:115`).
//!   - Mutation.deleteRole     — `conduit.resolvers.go:490` →
//!     `biz.RoleService.DeleteRole` (`biz/role.go:179`).
//!   - Mutation.bulkDeleteRoles — `conduit.resolvers.go:500` →
//!     `biz.RoleService.BulkDeleteRoles` (`biz/role.go:214`).
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! Cross-domain edge fields and `has<Edge>With` filters reference other
//! entities' `*WhereInput` types and belong to other slices:
//!
//!   - `Role.users(...)`, `Role.project`, `Role.userRoles` — edge fields into
//!     other entity domains.
//!   - `RoleWhereInput.hasUsersWith: [UserWhereInput!]`,
//!     `hasProjectWith: [ProjectWhereInput!]`,
//!     `hasUserRolesWith: [UserRoleWhereInput!]` — they reference other
//!     entities' WhereInput types.
//!   - The single-object `role(id: ID!)` lookup goes through the global
//!     `node(id: ID!)` Relay query (separate slice).

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::policy::AdminAccessScope;
use crate::scalars::{CursorScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; lowercase values are pinned explicitly
// because the default SCREAMING_SNAKE renaming would mangle them)
// ---------------------------------------------------------------------------

/// `enum RoleLevel { system project }` — snapshot line 6689, bound to Go
/// `ent/role.Level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum RoleLevel {
    #[graphql(name = "system")]
    System,
    #[graphql(name = "project")]
    Project,
}

/// `enum RoleOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 6711-6714 (two values only; roles have no NAME ordering, matching
/// ent's generated `OrderField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RoleOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

// ---------------------------------------------------------------------------
// Output object types
// ---------------------------------------------------------------------------

/// `type Role implements Node` — snapshot lines 6607-6659, scalar and
/// self-domain fields only. Cross-domain edge fields (`users(…)`,
/// `project`, `userRoles`) are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct Role {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub name: String,
    pub level: RoleLevel,
    // All-caps acronym tag: default camelCase would emit `projectId`.
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    pub scopes: Option<Vec<String>>,
}

/// `type RoleEdge { node: Role cursor: Cursor! }` — snapshot line 6678.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RoleEdge {
    pub node: Option<Role>,
    pub cursor: CursorScalar,
}

/// `type RoleConnection` — snapshot line 6661. `edges` is a nullable list
/// of nullable edges (`[RoleEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RoleConnection {
    pub edges: Option<Vec<Option<RoleEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input CreateRoleInput` — snapshot lines 3163-3177 (ent-generated).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreateRoleInput {
    pub name: String,
    pub level: Option<RoleLevel>,
    pub scopes: Option<Vec<String>>,
    #[graphql(name = "userIDs")]
    pub user_ids: Option<Vec<ID>>,
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
}

/// `input UpdateRoleInput` — snapshot lines 7622-7636 (ent-generated).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdateRoleInput {
    pub name: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub append_scopes: Option<Vec<String>>,
    pub clear_scopes: Option<bool>,
    #[graphql(name = "addUserIDs")]
    pub add_user_ids: Option<Vec<ID>>,
    #[graphql(name = "removeUserIDs")]
    pub remove_user_ids: Option<Vec<ID>>,
    pub clear_users: Option<bool>,
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    pub clear_project: Option<bool>,
}

/// `input RoleOrder { direction: OrderDirection! = ASC field:
/// RoleOrderField! }` — snapshot lines 6698-6707.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct RoleOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: RoleOrderField,
}

/// `input RoleWhereInput` — snapshot lines 6719-6793 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family, and the `has<Edge>: Boolean` existence predicates. The
/// `has<Edge>With: [<Other>WhereInput!]` fields are pending (they reference
/// other entities' WhereInputs — see module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct RoleWhereInput {
    pub not: Option<Box<RoleWhereInput>>,
    pub and: Option<Vec<RoleWhereInput>>,
    pub or: Option<Vec<RoleWhereInput>>,
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
    // level field predicates
    pub level: Option<RoleLevel>,
    #[graphql(name = "levelNEQ")]
    pub level_neq: Option<RoleLevel>,
    pub level_in: Option<Vec<RoleLevel>>,
    pub level_not_in: Option<Vec<RoleLevel>>,
    // project_id field predicates (acronym rename: projectID*)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDIsNil")]
    pub project_id_is_nil: Option<bool>,
    #[graphql(name = "projectIDNotNil")]
    pub project_id_not_nil: Option<bool>,
    // edge existence predicates (`has<Edge>With` variants pending — they
    // reference other entities' WhereInput types, see module doc)
    pub has_users: Option<bool>,
    pub has_project: Option<bool>,
    pub has_user_roles: Option<bool>,
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:462-464)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of
/// the GraphQL `RoleOrderField` enum — it is ent's `DefaultRoleOrder`
/// (order by primary key), which the Go resolver substitutes when the
/// client asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleOrderTerm {
    /// ent `DefaultRoleOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleOrderSelection {
    pub direction: OrderDirection,
    pub term: RoleOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.roles` (ent.resolvers.go:462-464): a `CREATED_AT`
/// request is remapped to `ent.DefaultRoleOrder` (order by ID) with the
/// requested direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_role_order(order_by: Option<RoleOrder>) -> Option<RoleOrderSelection> {
    order_by.map(|order| RoleOrderSelection {
        direction: order.direction,
        term: match order.field {
            RoleOrderField::CreatedAt => RoleOrderTerm::Id,
            RoleOrderField::UpdatedAt => RoleOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependencies:
// `r.client.Role` for the connection query and `r.roleService` for the
// CRUD mutations)
// ---------------------------------------------------------------------------

/// Error surface for the role services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("role", name)`
///     (`internal/pkg/xerrors/graphql.go:104`): `"%s name '%s' already exists"`.
///   - level/projectID mismatch — `biz/role.go:62-91`: "project ID is not
///     allowed for system roles" / "project ID is required for project roles".
///   - permission denied — `permissionValidator.CanGrantScopes`: wrapped as
///     `"permission denied: %w"`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RoleServiceError {
    #[error("role service is not available")]
    ServiceUnavailable,
    #[error("role name '{0}' already exists")]
    DuplicateName(String),
    #[error("project ID is not allowed for system roles")]
    ProjectIdOnSystemRole,
    #[error("project ID is required for project roles")]
    MissingProjectIdOnProjectRole,
    #[error("invalid role level")]
    InvalidLevel,
    #[error("invalid scope slug: {0}")]
    InvalidScopeSlug(String),
    #[error("scope '{0}' is not allowed for project roles")]
    ScopeNotAllowedForProjectRole(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("failed to create role: {0}")]
    Create(String),
    #[error("failed to update role: {0}")]
    Update(String),
    #[error("failed to delete role: {0}")]
    Delete(String),
    #[error("failed to bulk delete roles: {0}")]
    BulkDelete(String),
    #[error("failed to query roles: {0}")]
    Query(String),
}

/// Arguments for the `roles` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct RoleConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<RoleOrderSelection>,
    pub where_filter: Option<RoleWhereInput>,
}

/// Backs `Query.roles` (Go ent.resolvers.go:458: `r.client.Role.Query()
/// .Paginate(...)`).
#[async_trait::async_trait]
pub trait RoleQueryServices: Send + Sync {
    async fn roles(&self, args: RoleConnectionArgs) -> Result<RoleConnection, RoleServiceError>;

    async fn roles_with_access(
        &self,
        access: &AdminAccessScope,
        args: RoleConnectionArgs,
    ) -> Result<RoleConnection, RoleServiceError> {
        match access {
            AdminAccessScope::Global => self.roles(args).await,
            AdminAccessScope::Project(_) => Err(RoleServiceError::PermissionDenied(
                "project-scoped role listing is not supported by this service".to_owned(),
            )),
        }
    }
}

/// Backs the four CRUD mutations (Go `biz.RoleService`).
#[async_trait::async_trait]
pub trait RoleMutationServices: Send + Sync {
    /// Mirrors `RoleService.CreateRole` (biz/role.go:42).
    async fn create_role(&self, input: CreateRoleInput) -> Result<Role, RoleServiceError>;

    async fn create_role_with_access(
        &self,
        access: &AdminAccessScope,
        input: CreateRoleInput,
    ) -> Result<Role, RoleServiceError> {
        if let AdminAccessScope::Project(_) = access
            && (input.level != Some(RoleLevel::Project)
                || input
                    .project_id
                    .as_ref()
                    .is_none_or(|project_id| !access.allows_project(project_id.as_str())))
        {
            return Err(RoleServiceError::PermissionDenied(
                "project-scoped permission can only create roles in the authorized project"
                    .to_owned(),
            ));
        }
        self.create_role(input).await
    }

    /// Mirrors `RoleService.UpdateRole` (biz/role.go:115).
    async fn update_role(&self, id: &str, input: UpdateRoleInput)
    -> Result<Role, RoleServiceError>;

    async fn update_role_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        input: UpdateRoleInput,
    ) -> Result<Role, RoleServiceError> {
        match access {
            AdminAccessScope::Global => self.update_role(id, input).await,
            AdminAccessScope::Project(_) => Err(RoleServiceError::PermissionDenied(
                "project-scoped role updates require a scoped service boundary".to_owned(),
            )),
        }
    }

    /// Mirrors `RoleService.DeleteRole` (biz/role.go:179).
    async fn delete_role(&self, id: &str) -> Result<(), RoleServiceError>;

    async fn delete_role_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
    ) -> Result<(), RoleServiceError> {
        match access {
            AdminAccessScope::Global => self.delete_role(id).await,
            AdminAccessScope::Project(_) => Err(RoleServiceError::PermissionDenied(
                "project-scoped role deletion requires a scoped service boundary".to_owned(),
            )),
        }
    }

    /// Mirrors `RoleService.BulkDeleteRoles` (biz/role.go:214). Empty ids
    /// is a no-op (Go iterates the empty slice and returns nil).
    async fn bulk_delete_roles(&self, ids: Vec<String>) -> Result<(), RoleServiceError>;

    async fn bulk_delete_roles_with_access(
        &self,
        access: &AdminAccessScope,
        ids: Vec<String>,
    ) -> Result<(), RoleServiceError> {
        match access {
            AdminAccessScope::Global => self.bulk_delete_roles(ids).await,
            AdminAccessScope::Project(_) if ids.is_empty() => Ok(()),
            AdminAccessScope::Project(_) => Err(RoleServiceError::PermissionDenied(
                "project-scoped bulk role deletion requires a scoped service boundary".to_owned(),
            )),
        }
    }
}

pub(crate) fn role_query_services(ctx: &Context<'_>) -> Result<Arc<dyn RoleQueryServices>, String> {
    match ctx.data::<Arc<dyn RoleQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(RoleServiceError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn role_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn RoleMutationServices>, String> {
    match ctx.data::<Arc<dyn RoleMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(RoleServiceError::ServiceUnavailable.to_string()),
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
    use crate::sdl_parity::{assert_block_parity, snapshot_text};
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    #[derive(Default, Clone)]
    struct InMemoryRoleService {
        roles: Arc<Mutex<Vec<Role>>>,
        captured_query_args: Arc<Mutex<Vec<RoleConnectionArgs>>>,
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_role(id: i64, name: &str, level: RoleLevel) -> Role {
        Role {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            name: name.to_owned(),
            level,
            project_id: None,
            scopes: None,
        }
    }

    #[async_trait::async_trait]
    impl RoleQueryServices for InMemoryRoleService {
        async fn roles(
            &self,
            args: RoleConnectionArgs,
        ) -> Result<RoleConnection, RoleServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<Role> = lock(&self.roles).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        RoleOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        RoleOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
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
            Ok(RoleConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(RoleEdge {
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

        async fn roles_with_access(
            &self,
            access: &AdminAccessScope,
            args: RoleConnectionArgs,
        ) -> Result<RoleConnection, RoleServiceError> {
            let mut connection = self.roles(args).await?;
            if let AdminAccessScope::Project(_) = access {
                let edges = connection.edges.get_or_insert_default();
                edges.retain(|edge| {
                    edge.as_ref()
                        .and_then(|edge| edge.node.as_ref())
                        .is_some_and(|role| {
                            role.level == RoleLevel::Project
                                && role
                                    .project_id
                                    .as_ref()
                                    .is_some_and(|id| access.allows_project(id.as_str()))
                        })
                });
                connection.total_count = edges.len() as i64;
            }
            Ok(connection)
        }
    }

    #[async_trait::async_trait]
    impl RoleMutationServices for InMemoryRoleService {
        async fn create_role(&self, input: CreateRoleInput) -> Result<Role, RoleServiceError> {
            // biz/role.go:62-91: level/projectID consistency.
            match input.level {
                Some(RoleLevel::System) => {
                    if input.project_id.is_some() {
                        return Err(RoleServiceError::ProjectIdOnSystemRole);
                    }
                }
                Some(RoleLevel::Project) => {
                    if input.project_id.is_none() {
                        return Err(RoleServiceError::MissingProjectIdOnProjectRole);
                    }
                }
                None => {
                    if input.project_id.is_some() {
                        return Err(RoleServiceError::ProjectIdOnSystemRole);
                    }
                }
            }

            let level = input.level.unwrap_or(RoleLevel::System);

            let mut guard = lock(&self.roles);
            // biz/role.go:101-107: duplicate-name probe scoped to (level).
            if guard
                .iter()
                .any(|existing| existing.name == input.name && existing.level == level)
            {
                return Err(RoleServiceError::DuplicateName(input.name));
            }

            let id = guard.len() as i64 + 1;
            let created = Role {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                name: input.name,
                level,
                project_id: input.project_id,
                scopes: input.scopes,
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_role(
            &self,
            id: &str,
            input: UpdateRoleInput,
        ) -> Result<Role, RoleServiceError> {
            let mut guard = lock(&self.roles);
            let Some(role) = guard.iter_mut().find(|r| r.id.as_str() == id) else {
                return Err(RoleServiceError::Update(format!(
                    "role not found (id: {id})"
                )));
            };
            if let Some(v) = input.name {
                role.name = v;
            }
            if input.clear_scopes == Some(true) {
                role.scopes = None;
            } else if let Some(v) = input.scopes {
                role.scopes = Some(v);
            } else if let Some(append) = input.append_scopes {
                let mut current = role.scopes.clone().unwrap_or_default();
                current.extend(append);
                role.scopes = Some(current);
            }
            Ok(role.clone())
        }

        async fn update_role_with_access(
            &self,
            access: &AdminAccessScope,
            id: &str,
            input: UpdateRoleInput,
        ) -> Result<Role, RoleServiceError> {
            let allowed = lock(&self.roles).iter().any(|role| {
                role.id.as_str() == id
                    && role.level == RoleLevel::Project
                    && role
                        .project_id
                        .as_ref()
                        .is_some_and(|project_id| access.allows_project(project_id.as_str()))
            });
            if matches!(access, AdminAccessScope::Project(_)) && !allowed {
                return Err(RoleServiceError::PermissionDenied(
                    "role does not belong to the authorized project".to_owned(),
                ));
            }
            self.update_role(id, input).await
        }

        async fn delete_role(&self, id: &str) -> Result<(), RoleServiceError> {
            let mut guard = lock(&self.roles);
            let before = guard.len();
            guard.retain(|r| r.id.as_str() != id);
            if guard.len() == before {
                return Err(RoleServiceError::Delete(format!(
                    "role not found (id: {id})"
                )));
            }
            Ok(())
        }

        async fn delete_role_with_access(
            &self,
            access: &AdminAccessScope,
            id: &str,
        ) -> Result<(), RoleServiceError> {
            let allowed = lock(&self.roles).iter().any(|role| {
                role.id.as_str() == id
                    && role.level == RoleLevel::Project
                    && role
                        .project_id
                        .as_ref()
                        .is_some_and(|project_id| access.allows_project(project_id.as_str()))
            });
            if matches!(access, AdminAccessScope::Project(_)) && !allowed {
                return Err(RoleServiceError::PermissionDenied(
                    "role does not belong to the authorized project".to_owned(),
                ));
            }
            self.delete_role(id).await
        }

        async fn bulk_delete_roles(&self, ids: Vec<String>) -> Result<(), RoleServiceError> {
            // biz/role.go:214-253: empty ids is a no-op.
            if ids.is_empty() {
                return Ok(());
            }
            let mut guard = lock(&self.roles);
            let id_set: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
            guard.retain(|r| !id_set.contains(r.id.as_str()));
            Ok(())
        }
    }

    fn schema_with(store: &InMemoryRoleService) -> AdminSchema {
        let query: Arc<dyn RoleQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn RoleMutationServices> = Arc::new(store.clone());
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(system_context())
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        admin_schema_builder().data(system_context()).finish()
    }

    fn system_context() -> conduit_auth::RequestContext {
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(conduit_auth::Principal::system());
        context
    }

    fn project_ctx(project_id: &str) -> conduit_auth::request_context::RequestContext {
        let principal = conduit_auth::Principal::user("actor")
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::WRITE_ROLES,
            ))
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::READ_ROLES,
            ))
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::READ_USERS,
            ));
        let mut rc = conduit_auth::request_context::RequestContext::new();
        let _ = rc.set_principal(principal);
        let _ = rc.set_project_id(project_id);
        rc
    }

    // -----------------------------------------------------------------
    // SDL parity
    // -----------------------------------------------------------------

    #[test]
    fn sdl_role_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type Role",
            "type Role",
            &[
                "users(…): UserConnection!",
                "project: Project",
                "userRoles: [UserRole!]",
            ],
        )?;
        assert!(sdl.contains("type Role implements Node {"));
        assert!(snapshot.contains("type Role implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_role_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type RoleConnection",
            "type RoleConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type RoleEdge", "type RoleEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_role_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in ["enum RoleLevel", "enum RoleOrderField"] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_role_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input CreateRoleInput",
            "input UpdateRoleInput",
            "input RoleOrder",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        // Pin the ASC default exactly (snapshot line 6704).
        assert!(sdl.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    #[test]
    fn sdl_role_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input RoleWhereInput",
            "input RoleWhereInput",
            &[
                "hasUsersWith: [UserWhereInput!]",
                "hasProjectWith: [ProjectWhereInput!]",
                "hasUserRolesWith: [UserRoleWhereInput!]",
            ],
        )
    }

    #[test]
    fn sdl_roles_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query.roles signature (snapshot type Query, lines 5558-5587).
        assert!(
            sdl.contains(
                "roles(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: RoleOrder, where: RoleWhereInput): RoleConnection!"
            ),
            "generated SDL missing the roles connection signature: {sdl}"
        );

        // Mutations (snapshot type Mutation, lines 828-831).
        for signature in [
            "createRole(input: CreateRoleInput!): Role!",
            "updateRole(id: ID!, input: UpdateRoleInput!): Role!",
            "deleteRole(id: ID!): Boolean!",
            "bulkDeleteRoles(ids: [ID!]!): Boolean!",
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

    // -----------------------------------------------------------------
    // Ordering lowering
    // -----------------------------------------------------------------

    #[test]
    fn resolve_role_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_role_order(Some(RoleOrder {
            direction: OrderDirection::Desc,
            field: RoleOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(RoleOrderSelection {
                direction: OrderDirection::Desc,
                term: RoleOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_role_order_maps_updated_at_one_to_one() {
        let selection = resolve_role_order(Some(RoleOrder {
            direction: OrderDirection::Asc,
            field: RoleOrderField::UpdatedAt,
        }));
        assert_eq!(
            selection,
            Some(RoleOrderSelection {
                direction: OrderDirection::Asc,
                term: RoleOrderTerm::UpdatedAt,
            })
        );
        assert_eq!(resolve_role_order(None), None);
    }

    // -----------------------------------------------------------------
    // Resolver: createRole
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn create_role_system_level_with_defaults() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createRole(input: { name: "admin", scopes: ["read_users"] }) {
                        id name level scopes projectID
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let role = &data["createRole"];
        assert_eq!(role["id"], "1");
        assert_eq!(role["name"], "admin");
        // ent default: level = system (biz/role.go:67-71).
        assert_eq!(role["level"], "system");
        assert_eq!(role["scopes"][0], "read_users");
        assert!(role["projectID"].is_null());
        Ok(())
    }

    #[tokio::test]
    async fn create_role_project_level_requires_project_id() {
        let store = InMemoryRoleService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { createRole(input: { name: "p", level: project }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("project ID is required for project roles"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn create_role_system_level_rejects_project_id() {
        let store = InMemoryRoleService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createRole(input: { name: "p", level: system, projectID: "1" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("project ID is not allowed for system roles"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn create_role_duplicate_name_surfaces_go_error_message() {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(1, "dup", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { createRole(input: { name: "dup" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("role name 'dup' already exists"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: updateRole
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_role_applies_partial_merge() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(2, "old", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateRole(id: "2", input: { name: "new" }) { id name }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateRole"]["name"], "new");
        Ok(())
    }

    #[tokio::test]
    async fn update_role_missing_id_surfaces_wrapped_error() {
        let store = InMemoryRoleService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateRole(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update role"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: deleteRole / bulkDeleteRoles
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn delete_role_returns_true_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(5, "victim", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema.execute(r#"mutation { deleteRole(id: "5") }"#).await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteRole"], true);
        assert!(lock(&store.roles).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn bulk_delete_roles_returns_true_and_removes_matching() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(1, "a", RoleLevel::System));
        lock(&store.roles).push(sample_role(2, "b", RoleLevel::System));
        lock(&store.roles).push(sample_role(3, "c", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkDeleteRoles(ids: ["1", "3"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDeleteRoles"], true);
        let remaining: Vec<String> = lock(&store.roles)
            .iter()
            .map(|r| r.id.to_string())
            .collect();
        assert_eq!(remaining, vec!["2".to_owned()]);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: roles connection query
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn roles_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(1, "a", RoleLevel::System));
        lock(&store.roles).push(sample_role(2, "b", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    roles {
                        totalCount
                        edges { cursor node { id name level } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["roles"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn roles_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        lock(&store.roles).push(sample_role(1, "a", RoleLevel::System));
        lock(&store.roles).push(sample_role(2, "b", RoleLevel::System));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    roles(orderBy: { field: CREATED_AT, direction: DESC }) {
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
            Some(RoleOrderSelection {
                direction: OrderDirection::Desc,
                term: RoleOrderTerm::Id,
            })
        );
        let data = resp.data.into_json()?;
        assert_eq!(data["roles"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn roles_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ roles { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("role service is not available"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn project_role_grant_ceiling_allows_same_project_and_rejects_other_targets()
    -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        let schema = schema_with(&store);

        let same_project = schema
            .execute(
                async_graphql::Request::new(
                    r#"mutation {
                        createRole(input: {
                            name: "same-project"
                            level: project
                            projectID: "A"
                            scopes: ["read_users"]
                        }) { id projectID scopes }
                    }"#,
                )
                .data(project_ctx("A")),
            )
            .await;
        assert!(
            same_project.errors.is_empty(),
            "same-project grant failed: {:?}",
            same_project.errors
        );

        for mutation in [
            r#"mutation { createRole(input: { name: "project-b", level: project, projectID: "B", scopes: ["read_users"] }) { id } }"#,
            r#"mutation { createRole(input: { name: "system", level: system, scopes: ["read_users"] }) { id } }"#,
        ] {
            let response = schema
                .execute(async_graphql::Request::new(mutation).data(project_ctx("A")))
                .await;
            assert_eq!(response.errors.len(), 1);
            assert!(
                response.errors[0].message.contains("permission denied"),
                "unexpected error: {}",
                response.errors[0].message
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn project_scoped_role_operations_hide_system_and_project_b_roles()
    -> Result<(), TestError> {
        let store = InMemoryRoleService::default();
        let mut project_a = sample_role(1, "project-a", RoleLevel::Project);
        project_a.project_id = Some("A".into());
        let mut project_b = sample_role(2, "project-b", RoleLevel::Project);
        project_b.project_id = Some("B".into());
        let system = sample_role(3, "system", RoleLevel::System);
        lock(&store.roles).extend([project_a, project_b, system]);
        let schema = schema_with(&store);

        let listed = schema
            .execute(
                async_graphql::Request::new("{ roles { totalCount edges { node { name } } } }")
                    .data(project_ctx("A")),
            )
            .await;
        assert!(listed.errors.is_empty(), "errors: {:?}", listed.errors);
        let data = listed.data.into_json()?;
        assert_eq!(data["roles"]["totalCount"], 1);
        assert_eq!(data["roles"]["edges"][0]["node"]["name"], "project-a");

        let allowed = schema
            .execute(
                async_graphql::Request::new(
                    r#"mutation { updateRole(id: "1", input: { name: "updated-a" }) { name } }"#,
                )
                .data(project_ctx("A")),
            )
            .await;
        assert!(allowed.errors.is_empty(), "errors: {:?}", allowed.errors);

        for mutation in [
            r#"mutation { updateRole(id: "2", input: { name: "changed-b" }) { id } }"#,
            r#"mutation { deleteRole(id: "3") }"#,
        ] {
            let response = schema
                .execute(async_graphql::Request::new(mutation).data(project_ctx("A")))
                .await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("permission denied"));
        }
        assert_eq!(lock(&store.roles)[1].name, "project-b");
        assert_eq!(lock(&store.roles)[2].name, "system");
        Ok(())
    }
}

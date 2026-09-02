//! RUST-P12-001 S07 (Pauli-13th) — User-domain GraphQL slice.
//!
//! Bounded scope: the `users` connection query, the four User-domain
//! mutations declared in `conduit/internal/server/gql/conduit.graphql`
//! lines 823-826 (`createUser`/`updateUser`/`updateUserStatus`/`deleteUser`),
//! and the three project-link mutations on `Mutation` lines 839-841
//! (`addUserToProject` / `removeUserFromProject` / `updateProjectUser`)
//! — all six delegate to `biz.UserService` in Go
//! (`conduit.resolvers.go:456-560`). Every GraphQL type/input they
//! reference is copied field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type User implements Node` (snapshot line 8240) — scalar/self-domain
//!     fields only; cross-domain edge fields (`projects(…)`, `apiKeys(…)`,
//!     `roles(…)`, `channelOverrideTemplates(…)`, `oidcIdentities(…)`,
//!     `projectUsers`, `userRoles`) are pending (see module doc below).
//!   - `type UserConnection` / `type UserEdge` (lines 8422 / 8439).
//!   - `type UserProject implements Node` (line 8478) — included because
//!     `addUserToProject` / `updateProjectUser` return it.
//!   - `type UserRole implements Node` (line 8553) — referenced from
//!     `User.userRoles` (pending edge); declared here so the type exists
//!     for the SDL parity probe and the future edge field.
//!   - `enum UserStatus` (line 8631, two lowercase values: `activated`
//!     `deactivated`).
//!   - `enum UserOrderField` (line 8465) + `input UserOrder` (line 8452).
//!   - `input CreateUserInput` (lines 3293-3311, ent-generated).
//!   - `input UpdateUserInput` (lines 7747-7781, ent-generated).
//!   - `input AddUserToProjectInput` / `input UpdateProjectUserInput` /
//!     `input RemoveUserFromProjectInput` (lines 526-548, hand-written).
//!   - `input UserWhereInput` (lines 8639-8801, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `has<Edge>: Boolean`.
//!
//! Go reference implementations:
//!   - Query.users              — `internal/server/gql/ent.resolvers.go:534`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultUserOrder` = ID before
//!     delegating to ent `Paginate`).
//!   - Mutation.createUser      — `conduit.resolvers.go:456` →
//!     `biz.UserService.CreateUser` (`biz/user.go:48`): hash password
//!     (unless OIDC-only placeholder), SetNillableFirstName/LastName,
//!     SetEmail, SetScopes, AddRoleIDs.
//!   - Mutation.updateUser      — `conduit.resolvers.go:461` →
//!     `biz.UserService.UpdateUser` (`biz/user.go:84`).
//!   - Mutation.updateUserStatus — `conduit.resolvers.go:466` →
//!     `biz.UserService.UpdateUserStatus` (`biz/user.go:210`).
//!   - Mutation.deleteUser      — `conduit.resolvers.go:471` →
//!     `biz.UserService.DeleteUser` (`biz/user.go:533`); resolver returns
//!     `false, err` on failure, `true` on success.
//!   - Mutation.addUserToProject — `conduit.resolvers.go:541` →
//!     `biz.UserService.AddUserToProject` (`biz/user.go:367`).
//!   - Mutation.removeUserFromProject — `conduit.resolvers.go:547` →
//!     `biz.UserService.RemoveUserFromProject` (`biz/user.go:408`); resolver
//!     returns `false, err` / `true`.
//!   - Mutation.updateProjectUser — `conduit.resolvers.go:557` →
//!     `biz.UserService.UpdateProjectUser` (`biz/user.go:447`).
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! Cross-domain edge fields and `has<Edge>With` filters reference other
//! entities' `*WhereInput` types and belong to other slices:
//!
//!   - `User.projects(...)`, `User.apiKeys(...)`, `User.roles(...)`,
//!     `User.channelOverrideTemplates(...)`, `User.oidcIdentities(...)`,
//!     `User.projectUsers`, `User.userRoles` — edge fields into other
//!     entity domains.
//!   - `UserWhereInput.hasProjectsWith: [ProjectWhereInput!]`,
//!     `hasAPIKeysWith: [APIKeyWhereInput!]`,
//!     `hasRolesWith: [RoleWhereInput!]`,
//!     `hasChannelOverrideTemplatesWith`,
//!     `hasOidcIdentitiesWith`,
//!     `hasProjectUsersWith: [UserProjectWhereInput!]`,
//!     `hasUserRolesWith: [UserRoleWhereInput!]` — they reference other
//!     entities' WhereInput types.

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::policy::AdminAccessScope;
use crate::role::{
    RoleConnection, RoleConnectionArgs, RoleOrder, RoleWhereInput, resolve_role_order,
};
use crate::scalars::{CursorScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; lowercase values are pinned explicitly)
// ---------------------------------------------------------------------------

/// `enum UserStatus { activated deactivated }` — snapshot line 8631, bound
/// to Go `ent/user.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Enum)]
pub enum UserStatus {
    #[graphql(name = "activated")]
    Activated,
    #[graphql(name = "deactivated")]
    Deactivated,
}

/// `enum UserOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 8465-8468.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum UserOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

// ---------------------------------------------------------------------------
// Output object types
// ---------------------------------------------------------------------------

/// `type User implements Node` — snapshot lines 8240-8421, scalar and
/// self-domain fields only. Cross-domain edge fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct User {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub email: String,
    pub status: UserStatus,
    pub prefer_language: String,
    pub first_name: String,
    pub last_name: String,
    /// 用户头像URL — nullable in the contract (snapshot line 8258).
    pub avatar: Option<String>,
    pub is_owner: bool,
    /// User scopes in system level.
    pub scopes: Option<Vec<String>>,
}

#[ComplexObject]
impl User {
    /// Roles currently assigned to this user. This is a real relation lookup
    /// through the injected user service, so mutation payloads and list/detail
    /// queries all expose the same up-to-date role edges the frontend expects.
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
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_ROLES)
            .map_err(|error| error.to_string())?;
        let services = user_query_services(ctx)?;
        let access =
            AdminAccessScope::from_graphql_context(ctx, conduit_auth::scopes::slug::READ_ROLES)
                .map_err(|error| error.to_string())?;
        services
            .roles_for_user_with_access(
                &access,
                self.id.as_str(),
                RoleConnectionArgs {
                    after: after.map(|cursor| cursor.0),
                    first,
                    before: before.map(|cursor| cursor.0),
                    last,
                    order_by: resolve_role_order(order_by),
                    where_filter,
                },
            )
            .await
            .map_err(|err| err.to_string())
    }
}

/// `type UserEdge { node: User cursor: Cursor! }` — snapshot line 8439.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UserEdge {
    pub node: Option<User>,
    pub cursor: CursorScalar,
}

/// `type UserConnection` — snapshot line 8422. `edges` is a nullable list
/// of nullable edges (`[UserEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UserConnection {
    pub edges: Option<Vec<Option<UserEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `type UserProject implements Node` — snapshot lines 8478-8501. Returned
/// by `addUserToProject` / `updateProjectUser`. Cross-domain `user` /
/// `project` edge fields are pending.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct UserProject {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    #[graphql(name = "userID")]
    pub user_id: ID,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub is_owner: bool,
    pub scopes: Option<Vec<String>>,
}

#[ComplexObject]
impl UserProject {
    async fn user(&self, ctx: &Context<'_>) -> Result<User, String> {
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_USERS)
            .map_err(|error| error.to_string())?;
        let access =
            AdminAccessScope::from_graphql_context(ctx, conduit_auth::scopes::slug::READ_USERS)
                .map_err(|error| error.to_string())?;
        let connection = user_query_services(ctx)?
            .users_with_access(
                &access,
                UserConnectionArgs {
                    where_filter: Some(UserWhereInput {
                        id: Some(self.user_id.clone()),
                        ..UserWhereInput::default()
                    }),
                    first: Some(1),
                    ..UserConnectionArgs::default()
                },
            )
            .await
            .map_err(|err| err.to_string())?;
        connection
            .edges
            .into_iter()
            .flatten()
            .flatten()
            .find_map(|edge| edge.node)
            .ok_or_else(|| UserServiceError::NotFound(self.user_id.to_string()).to_string())
    }
}

/// `type UserRole implements Node` — snapshot lines 8553-8562. Declared
/// here so the type is registered for the (pending) `User.userRoles` edge
/// and the SDL parity probe. Note: `createdAt` / `updatedAt` are NULLABLE
/// in the contract (snapshot lines 8557-8558 declare `Time`, not `Time!`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UserRole {
    pub id: ID,
    #[graphql(name = "userID")]
    pub user_id: ID,
    #[graphql(name = "roleID")]
    pub role_id: ID,
    pub created_at: Option<TimeScalar>,
    pub updated_at: Option<TimeScalar>,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input CreateUserInput` — snapshot lines 3293-3311 (ent-generated).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CreateUserInput {
    pub email: String,
    pub status: Option<UserStatus>,
    /// 用户偏好语言.
    pub prefer_language: Option<String>,
    pub password: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// 用户头像URL.
    pub avatar: Option<String>,
    pub is_owner: Option<bool>,
    /// User scopes in system level.
    pub scopes: Option<Vec<String>>,
    #[graphql(name = "projectIDs")]
    pub project_ids: Option<Vec<ID>>,
    #[graphql(name = "roleIDs")]
    pub role_ids: Option<Vec<ID>>,
}

/// `input UpdateUserInput` — snapshot lines 7747-7781 (ent-generated).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct UpdateUserInput {
    pub email: Option<String>,
    pub status: Option<UserStatus>,
    pub prefer_language: Option<String>,
    pub password: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub avatar: Option<String>,
    pub clear_avatar: Option<bool>,
    pub is_owner: Option<bool>,
    pub scopes: Option<Vec<String>>,
    pub append_scopes: Option<Vec<String>>,
    pub clear_scopes: Option<bool>,
    #[graphql(name = "addProjectIDs")]
    pub add_project_ids: Option<Vec<ID>>,
    #[graphql(name = "removeProjectIDs")]
    pub remove_project_ids: Option<Vec<ID>>,
    pub clear_projects: Option<bool>,
    #[graphql(name = "addRoleIDs")]
    pub add_role_ids: Option<Vec<ID>>,
    #[graphql(name = "removeRoleIDs")]
    pub remove_role_ids: Option<Vec<ID>>,
    pub clear_roles: Option<bool>,
}

/// `input AddUserToProjectInput` — snapshot lines 526-533 (hand-written in
/// the Go schema, NOT ent-generated). Note the lowercase `projectId` /
/// `userId` fields — these are the hand-written camelCase tags, distinct
/// from the ent-generated acronym tags (`projectID`) used elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct AddUserToProjectInput {
    pub project_id: ID,
    pub user_id: ID,
    pub is_owner: Option<bool>,
    pub scopes: Option<Vec<String>>,
    #[graphql(name = "roleIDs")]
    pub role_ids: Option<Vec<ID>>,
}

/// `input UpdateProjectUserInput` — snapshot lines 534-542.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct UpdateProjectUserInput {
    pub project_id: ID,
    pub user_id: ID,
    pub is_owner: Option<bool>,
    pub scopes: Option<Vec<String>>,
    #[graphql(name = "addRoleIDs")]
    pub add_role_ids: Option<Vec<ID>>,
    #[graphql(name = "removeRoleIDs")]
    pub remove_role_ids: Option<Vec<ID>>,
}

/// `input RemoveUserFromProjectInput` — snapshot lines 543-547.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct RemoveUserFromProjectInput {
    pub project_id: ID,
    pub user_id: ID,
}

/// `input UserOrder { direction: OrderDirection! = ASC field:
/// UserOrderField! }` — snapshot lines 8452-8461.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct UserOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: UserOrderField,
}

/// `input UserWhereInput` — snapshot lines 8639-8801 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family, and the `has<Edge>: Boolean` existence predicates.
/// The `has<Edge>With` fields are pending (module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct UserWhereInput {
    pub not: Option<Box<UserWhereInput>>,
    pub and: Option<Vec<UserWhereInput>>,
    pub or: Option<Vec<UserWhereInput>>,
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
    // email field predicates
    pub email: Option<String>,
    #[graphql(name = "emailNEQ")]
    pub email_neq: Option<String>,
    pub email_in: Option<Vec<String>>,
    pub email_not_in: Option<Vec<String>>,
    #[graphql(name = "emailGT")]
    pub email_gt: Option<String>,
    #[graphql(name = "emailGTE")]
    pub email_gte: Option<String>,
    #[graphql(name = "emailLT")]
    pub email_lt: Option<String>,
    #[graphql(name = "emailLTE")]
    pub email_lte: Option<String>,
    pub email_contains: Option<String>,
    pub email_has_prefix: Option<String>,
    pub email_has_suffix: Option<String>,
    pub email_equal_fold: Option<String>,
    pub email_contains_fold: Option<String>,
    // status field predicates
    pub status: Option<UserStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<UserStatus>,
    pub status_in: Option<Vec<UserStatus>>,
    pub status_not_in: Option<Vec<UserStatus>>,
    // prefer_language field predicates
    pub prefer_language: Option<String>,
    #[graphql(name = "preferLanguageNEQ")]
    pub prefer_language_neq: Option<String>,
    pub prefer_language_in: Option<Vec<String>>,
    pub prefer_language_not_in: Option<Vec<String>>,
    #[graphql(name = "preferLanguageGT")]
    pub prefer_language_gt: Option<String>,
    #[graphql(name = "preferLanguageGTE")]
    pub prefer_language_gte: Option<String>,
    #[graphql(name = "preferLanguageLT")]
    pub prefer_language_lt: Option<String>,
    #[graphql(name = "preferLanguageLTE")]
    pub prefer_language_lte: Option<String>,
    pub prefer_language_contains: Option<String>,
    pub prefer_language_has_prefix: Option<String>,
    pub prefer_language_has_suffix: Option<String>,
    pub prefer_language_equal_fold: Option<String>,
    pub prefer_language_contains_fold: Option<String>,
    // first_name field predicates
    pub first_name: Option<String>,
    #[graphql(name = "firstNameNEQ")]
    pub first_name_neq: Option<String>,
    pub first_name_in: Option<Vec<String>>,
    pub first_name_not_in: Option<Vec<String>>,
    #[graphql(name = "firstNameGT")]
    pub first_name_gt: Option<String>,
    #[graphql(name = "firstNameGTE")]
    pub first_name_gte: Option<String>,
    #[graphql(name = "firstNameLT")]
    pub first_name_lt: Option<String>,
    #[graphql(name = "firstNameLTE")]
    pub first_name_lte: Option<String>,
    pub first_name_contains: Option<String>,
    pub first_name_has_prefix: Option<String>,
    pub first_name_has_suffix: Option<String>,
    pub first_name_equal_fold: Option<String>,
    pub first_name_contains_fold: Option<String>,
    // last_name field predicates
    pub last_name: Option<String>,
    #[graphql(name = "lastNameNEQ")]
    pub last_name_neq: Option<String>,
    pub last_name_in: Option<Vec<String>>,
    pub last_name_not_in: Option<Vec<String>>,
    #[graphql(name = "lastNameGT")]
    pub last_name_gt: Option<String>,
    #[graphql(name = "lastNameGTE")]
    pub last_name_gte: Option<String>,
    #[graphql(name = "lastNameLT")]
    pub last_name_lt: Option<String>,
    #[graphql(name = "lastNameLTE")]
    pub last_name_lte: Option<String>,
    pub last_name_contains: Option<String>,
    pub last_name_has_prefix: Option<String>,
    pub last_name_has_suffix: Option<String>,
    pub last_name_equal_fold: Option<String>,
    pub last_name_contains_fold: Option<String>,
    // avatar field predicates (avatar is nullable → adds *IsNil / *NotNil)
    pub avatar: Option<String>,
    #[graphql(name = "avatarNEQ")]
    pub avatar_neq: Option<String>,
    pub avatar_in: Option<Vec<String>>,
    pub avatar_not_in: Option<Vec<String>>,
    #[graphql(name = "avatarGT")]
    pub avatar_gt: Option<String>,
    #[graphql(name = "avatarGTE")]
    pub avatar_gte: Option<String>,
    #[graphql(name = "avatarLT")]
    pub avatar_lt: Option<String>,
    #[graphql(name = "avatarLTE")]
    pub avatar_lte: Option<String>,
    pub avatar_contains: Option<String>,
    pub avatar_has_prefix: Option<String>,
    pub avatar_has_suffix: Option<String>,
    pub avatar_is_nil: Option<bool>,
    pub avatar_not_nil: Option<bool>,
    pub avatar_equal_fold: Option<String>,
    pub avatar_contains_fold: Option<String>,
    // is_owner field predicates
    pub is_owner: Option<bool>,
    #[graphql(name = "isOwnerNEQ")]
    pub is_owner_neq: Option<bool>,
    // edge existence predicates (`has<Edge>With` variants pending — they
    // reference other entities' WhereInput types, see module doc). Note the
    // mixed casing: `hasAPIKeys` (acronym) vs `hasOidcIdentities` (not).
    pub has_projects: Option<bool>,
    #[graphql(name = "hasAPIKeys")]
    pub has_api_keys: Option<bool>,
    pub has_roles: Option<bool>,
    pub has_channel_override_templates: Option<bool>,
    pub has_oidc_identities: Option<bool>,
    pub has_project_users: Option<bool>,
    pub has_user_roles: Option<bool>,
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:538-540)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of
/// the GraphQL `UserOrderField` enum — it is ent's `DefaultUserOrder`
/// (order by primary key), which the Go resolver substitutes when the
/// client asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOrderTerm {
    /// ent `DefaultUserOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserOrderSelection {
    pub direction: OrderDirection,
    pub term: UserOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.users` (ent.resolvers.go:538-540): a `CREATED_AT`
/// request is remapped to `ent.DefaultUserOrder` (order by ID) with the
/// requested direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_user_order(order_by: Option<UserOrder>) -> Option<UserOrderSelection> {
    order_by.map(|order| UserOrderSelection {
        direction: order.direction,
        term: match order.field {
            UserOrderField::CreatedAt => UserOrderTerm::Id,
            UserOrderField::UpdatedAt => UserOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependency on
// `r.userService` and `r.client.User`)
// ---------------------------------------------------------------------------

/// Error surface for the user services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - not found — `biz/user.go:230-264`: `"user not found (id: %d)"`.
///   - duplicate email — ent unique-constraint violation wrapped as
///     `"failed to create user: %w"`.
///   - permission denied — `permission_validator` wrapped as
///     `"permission denied: %w"`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum UserServiceError {
    #[error("user service is not available")]
    ServiceUnavailable,
    #[error("user not found (id: {0})")]
    NotFound(String),
    #[error("email '{0}' already exists")]
    DuplicateEmail(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("unsupported user input fields: {0}")]
    UnsupportedFields(String),
    #[error("failed to create user: {0}")]
    Create(String),
    #[error("failed to update user: {0}")]
    Update(String),
    #[error("failed to update user status: {0}")]
    UpdateStatus(String),
    #[error("failed to delete user: {0}")]
    Delete(String),
    #[error("failed to add user to project: {0}")]
    AddToProject(String),
    #[error("failed to remove user from project: {0}")]
    RemoveFromProject(String),
    #[error("failed to update project user: {0}")]
    UpdateProjectUser(String),
    #[error("failed to query users: {0}")]
    Query(String),
}

/// Arguments for the `users` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct UserConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<UserOrderSelection>,
    pub where_filter: Option<UserWhereInput>,
}

/// Backs `Query.users` (Go ent.resolvers.go:534: `r.client.User.Query()
/// .Paginate(...)`).
#[async_trait::async_trait]
pub trait UserQueryServices: Send + Sync {
    async fn users(&self, args: UserConnectionArgs) -> Result<UserConnection, UserServiceError>;

    async fn users_with_access(
        &self,
        access: &AdminAccessScope,
        args: UserConnectionArgs,
    ) -> Result<UserConnection, UserServiceError> {
        match access {
            AdminAccessScope::Global => self.users(args).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped user listing is not supported by this service".to_owned(),
            )),
        }
    }

    async fn roles_for_user(
        &self,
        user_id: &str,
        args: RoleConnectionArgs,
    ) -> Result<RoleConnection, UserServiceError>;

    async fn roles_for_user_with_access(
        &self,
        access: &AdminAccessScope,
        user_id: &str,
        args: RoleConnectionArgs,
    ) -> Result<RoleConnection, UserServiceError> {
        match access {
            AdminAccessScope::Global => self.roles_for_user(user_id, args).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped role listing is not supported by this service".to_owned(),
            )),
        }
    }

    async fn project_users(&self, project_id: &str) -> Result<Vec<UserProject>, UserServiceError>;

    async fn project_users_with_access(
        &self,
        access: &AdminAccessScope,
        project_id: &str,
    ) -> Result<Vec<UserProject>, UserServiceError> {
        if !access.allows_project(project_id) {
            return Err(UserServiceError::PermissionDenied(
                "project does not match the authorized project".to_owned(),
            ));
        }
        self.project_users(project_id).await
    }
}

/// Backs the six User-domain mutations (Go `biz.UserService`).
#[async_trait::async_trait]
pub trait UserMutationServices: Send + Sync {
    /// Mirrors `UserService.CreateUser` (biz/user.go:48).
    async fn create_user(&self, input: CreateUserInput) -> Result<User, UserServiceError>;

    async fn create_user_with_access(
        &self,
        access: &AdminAccessScope,
        input: CreateUserInput,
    ) -> Result<User, UserServiceError> {
        match access {
            AdminAccessScope::Global => self.create_user(input).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped permission cannot create global users".to_owned(),
            )),
        }
    }

    /// Mirrors `UserService.UpdateUser` (biz/user.go:84).
    async fn update_user(&self, id: &str, input: UpdateUserInput)
    -> Result<User, UserServiceError>;

    async fn update_user_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        input: UpdateUserInput,
    ) -> Result<User, UserServiceError> {
        match access {
            AdminAccessScope::Global => self.update_user(id, input).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped permission cannot update global users".to_owned(),
            )),
        }
    }

    /// Mirrors `UserService.UpdateUserStatus` (biz/user.go:210).
    async fn update_user_status(
        &self,
        id: &str,
        status: UserStatus,
    ) -> Result<User, UserServiceError>;

    async fn update_user_status_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
        status: UserStatus,
    ) -> Result<User, UserServiceError> {
        match access {
            AdminAccessScope::Global => self.update_user_status(id, status).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped permission cannot update global user status".to_owned(),
            )),
        }
    }

    /// Mirrors `UserService.DeleteUser` (biz/user.go:533).
    async fn delete_user(&self, id: &str) -> Result<(), UserServiceError>;

    async fn delete_user_with_access(
        &self,
        access: &AdminAccessScope,
        id: &str,
    ) -> Result<(), UserServiceError> {
        match access {
            AdminAccessScope::Global => self.delete_user(id).await,
            AdminAccessScope::Project(_) => Err(UserServiceError::PermissionDenied(
                "project-scoped permission cannot delete global users".to_owned(),
            )),
        }
    }

    /// Mirrors `UserService.AddUserToProject` (biz/user.go:367).
    async fn add_user_to_project(
        &self,
        input: AddUserToProjectInput,
    ) -> Result<UserProject, UserServiceError>;

    async fn add_user_to_project_with_access(
        &self,
        access: &AdminAccessScope,
        input: AddUserToProjectInput,
    ) -> Result<UserProject, UserServiceError> {
        if !access.allows_project(input.project_id.as_str()) {
            return Err(UserServiceError::PermissionDenied(
                "project does not match the authorized project".to_owned(),
            ));
        }
        self.add_user_to_project(input).await
    }

    /// Mirrors `UserService.RemoveUserFromProject` (biz/user.go:408).
    async fn remove_user_from_project(
        &self,
        input: RemoveUserFromProjectInput,
    ) -> Result<(), UserServiceError>;

    async fn remove_user_from_project_with_access(
        &self,
        access: &AdminAccessScope,
        input: RemoveUserFromProjectInput,
    ) -> Result<(), UserServiceError> {
        if !access.allows_project(input.project_id.as_str()) {
            return Err(UserServiceError::PermissionDenied(
                "project does not match the authorized project".to_owned(),
            ));
        }
        self.remove_user_from_project(input).await
    }

    /// Mirrors `UserService.UpdateProjectUser` (biz/user.go:447).
    async fn update_project_user(
        &self,
        input: UpdateProjectUserInput,
    ) -> Result<UserProject, UserServiceError>;

    async fn update_project_user_with_access(
        &self,
        access: &AdminAccessScope,
        input: UpdateProjectUserInput,
    ) -> Result<UserProject, UserServiceError> {
        if !access.allows_project(input.project_id.as_str()) {
            return Err(UserServiceError::PermissionDenied(
                "project does not match the authorized project".to_owned(),
            ));
        }
        self.update_project_user(input).await
    }
}

pub fn validate_create_user_input(input: &CreateUserInput) -> Result<(), UserServiceError> {
    let _ = input;
    Ok(())
}

pub fn validate_update_user_input(input: &UpdateUserInput) -> Result<(), UserServiceError> {
    let _ = input;
    Ok(())
}

pub(crate) fn user_query_services(ctx: &Context<'_>) -> Result<Arc<dyn UserQueryServices>, String> {
    match ctx.data::<Arc<dyn UserQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(UserServiceError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn user_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn UserMutationServices>, String> {
    match ctx.data::<Arc<dyn UserMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(UserServiceError::ServiceUnavailable.to_string()),
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
    struct InMemoryUserService {
        users: Arc<Mutex<Vec<User>>>,
        // user_id + project_id → UserProject row.
        user_projects: Arc<Mutex<Vec<UserProject>>>,
        captured_query_args: Arc<Mutex<Vec<UserConnectionArgs>>>,
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_user(id: i64, email: &str) -> User {
        User {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            email: email.to_owned(),
            status: UserStatus::Activated,
            prefer_language: "en".to_owned(),
            first_name: String::new(),
            last_name: String::new(),
            avatar: None,
            is_owner: false,
            scopes: None,
        }
    }

    #[derive(Clone)]
    struct FixedRoleQueryService {
        role: crate::role::Role,
    }

    #[async_trait::async_trait]
    impl crate::role::RoleQueryServices for FixedRoleQueryService {
        async fn roles(
            &self,
            _args: crate::role::RoleConnectionArgs,
        ) -> Result<crate::role::RoleConnection, crate::role::RoleServiceError> {
            Ok(crate::role::RoleConnection {
                edges: Some(vec![Some(crate::role::RoleEdge {
                    node: Some(self.role.clone()),
                    cursor: CursorScalar("0".to_owned()),
                })]),
                page_info: PageInfo::empty(false, false),
                total_count: 1,
            })
        }
    }

    #[async_trait::async_trait]
    impl UserQueryServices for InMemoryUserService {
        async fn users(
            &self,
            args: UserConnectionArgs,
        ) -> Result<UserConnection, UserServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<User> = lock(&self.users).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        UserOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        UserOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
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
            Ok(UserConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(UserEdge {
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

        async fn users_with_access(
            &self,
            access: &AdminAccessScope,
            args: UserConnectionArgs,
        ) -> Result<UserConnection, UserServiceError> {
            let mut connection = self.users(args).await?;
            if let AdminAccessScope::Project(_) = access {
                let member_ids: std::collections::HashSet<String> = lock(&self.user_projects)
                    .iter()
                    .filter(|membership| access.allows_project(membership.project_id.as_str()))
                    .map(|membership| membership.user_id.to_string())
                    .collect();
                let edges = connection.edges.get_or_insert_default();
                edges.retain(|edge| {
                    edge.as_ref()
                        .and_then(|edge| edge.node.as_ref())
                        .is_some_and(|user| member_ids.contains(user.id.as_str()))
                });
                connection.total_count = edges.len() as i64;
            }
            Ok(connection)
        }

        async fn roles_for_user(
            &self,
            _user_id: &str,
            _args: RoleConnectionArgs,
        ) -> Result<RoleConnection, UserServiceError> {
            Ok(RoleConnection {
                edges: Some(Vec::new()),
                page_info: PageInfo::empty(false, false),
                total_count: 0,
            })
        }

        async fn project_users(
            &self,
            project_id: &str,
        ) -> Result<Vec<UserProject>, UserServiceError> {
            Ok(lock(&self.user_projects)
                .iter()
                .filter(|membership| membership.project_id.as_str() == project_id)
                .cloned()
                .collect())
        }
    }

    #[async_trait::async_trait]
    impl UserMutationServices for InMemoryUserService {
        async fn create_user(&self, input: CreateUserInput) -> Result<User, UserServiceError> {
            let project_ids = input.project_ids.clone().unwrap_or_default();
            let mut guard = lock(&self.users);
            // ent unique constraint: email.
            if guard.iter().any(|u| u.email == input.email) {
                return Err(UserServiceError::DuplicateEmail(input.email));
            }
            let id = guard.len() as i64 + 1;
            // biz/user.go:60-74: SetNillable* applied; status left to ent
            // default ("activated") when None — the host wires that.
            let created = User {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                email: input.email,
                status: input.status.unwrap_or(UserStatus::Activated),
                prefer_language: input.prefer_language.unwrap_or_else(|| "en".to_owned()),
                first_name: input.first_name.unwrap_or_default(),
                last_name: input.last_name.unwrap_or_default(),
                avatar: input.avatar,
                is_owner: input.is_owner.unwrap_or(false),
                scopes: input.scopes,
            };
            guard.push(created.clone());
            drop(guard);
            let mut memberships = lock(&self.user_projects);
            for project_id in project_ids {
                if memberships.iter().any(|membership| {
                    membership.user_id == created.id && membership.project_id == project_id
                }) {
                    continue;
                }
                let membership_id = memberships.len() as i64 + 1;
                memberships.push(UserProject {
                    id: ID::from(membership_id.to_string()),
                    created_at: epoch(),
                    updated_at: epoch(),
                    user_id: created.id.clone(),
                    project_id,
                    is_owner: false,
                    scopes: None,
                });
            }
            Ok(created)
        }

        async fn update_user(
            &self,
            id: &str,
            input: UpdateUserInput,
        ) -> Result<User, UserServiceError> {
            let add_project_ids = input.add_project_ids.clone().unwrap_or_default();
            let remove_project_ids = input.remove_project_ids.clone().unwrap_or_default();
            let clear_projects = input.clear_projects == Some(true);
            let mut guard = lock(&self.users);
            let Some(user) = guard.iter_mut().find(|u| u.id.as_str() == id) else {
                return Err(UserServiceError::Update(
                    UserServiceError::NotFound(id.to_string()).to_string(),
                ));
            };
            if let Some(v) = input.email {
                user.email = v;
            }
            if let Some(v) = input.status {
                user.status = v;
            }
            if let Some(v) = input.prefer_language {
                user.prefer_language = v;
            }
            if let Some(v) = input.first_name {
                user.first_name = v;
            }
            if let Some(v) = input.last_name {
                user.last_name = v;
            }
            if input.clear_avatar == Some(true) {
                user.avatar = None;
            } else if let Some(v) = input.avatar {
                user.avatar = Some(v);
            }
            if let Some(v) = input.is_owner {
                user.is_owner = v;
            }
            if input.clear_scopes == Some(true) {
                user.scopes = Some(Vec::new());
            } else if input.scopes.is_some() || input.append_scopes.is_some() {
                let mut scopes = input
                    .scopes
                    .unwrap_or_else(|| user.scopes.clone().unwrap_or_default());
                scopes.extend(input.append_scopes.unwrap_or_default());
                user.scopes = Some(scopes);
            }
            let updated = user.clone();
            drop(guard);

            let mut memberships = lock(&self.user_projects);
            if clear_projects {
                memberships.retain(|membership| membership.user_id.as_str() != id);
            } else {
                for project_id in add_project_ids {
                    if memberships.iter().any(|membership| {
                        membership.user_id.as_str() == id && membership.project_id == project_id
                    }) {
                        continue;
                    }
                    let membership_id = memberships.len() as i64 + 1;
                    memberships.push(UserProject {
                        id: ID::from(membership_id.to_string()),
                        created_at: epoch(),
                        updated_at: epoch(),
                        user_id: updated.id.clone(),
                        project_id,
                        is_owner: false,
                        scopes: None,
                    });
                }
                memberships.retain(|membership| {
                    membership.user_id.as_str() != id
                        || !remove_project_ids.contains(&membership.project_id)
                });
            }
            Ok(updated)
        }

        async fn update_user_status(
            &self,
            id: &str,
            status: UserStatus,
        ) -> Result<User, UserServiceError> {
            let mut guard = lock(&self.users);
            let Some(user) = guard.iter_mut().find(|u| u.id.as_str() == id) else {
                return Err(UserServiceError::UpdateStatus(
                    UserServiceError::NotFound(id.to_string()).to_string(),
                ));
            };
            user.status = status;
            Ok(user.clone())
        }

        async fn delete_user(&self, id: &str) -> Result<(), UserServiceError> {
            let mut guard = lock(&self.users);
            let before = guard.len();
            guard.retain(|u| u.id.as_str() != id);
            if guard.len() == before {
                return Err(UserServiceError::Delete(
                    UserServiceError::NotFound(id.to_string()).to_string(),
                ));
            }
            Ok(())
        }

        async fn add_user_to_project(
            &self,
            input: AddUserToProjectInput,
        ) -> Result<UserProject, UserServiceError> {
            let mut guard = lock(&self.user_projects);
            // biz/user.go:380-406: duplicate (userid,projectid) is rejected.
            if guard
                .iter()
                .any(|up| up.user_id == input.user_id && up.project_id == input.project_id)
            {
                return Err(UserServiceError::AddToProject(format!(
                    "user {} is already a member of project {}",
                    input.user_id.as_str(),
                    input.project_id.as_str()
                )));
            }
            let id = guard.len() as i64 + 1;
            let created = UserProject {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                user_id: input.user_id,
                project_id: input.project_id,
                is_owner: input.is_owner.unwrap_or(false),
                scopes: input.scopes,
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn remove_user_from_project(
            &self,
            input: RemoveUserFromProjectInput,
        ) -> Result<(), UserServiceError> {
            let mut guard = lock(&self.user_projects);
            let before = guard.len();
            guard.retain(|up| !(up.user_id == input.user_id && up.project_id == input.project_id));
            if guard.len() == before {
                return Err(UserServiceError::RemoveFromProject(format!(
                    "user {} is not a member of project {}",
                    input.user_id.as_str(),
                    input.project_id.as_str()
                )));
            }
            Ok(())
        }

        async fn update_project_user(
            &self,
            input: UpdateProjectUserInput,
        ) -> Result<UserProject, UserServiceError> {
            let mut guard = lock(&self.user_projects);
            let Some(up) = guard
                .iter_mut()
                .find(|up| up.user_id == input.user_id && up.project_id == input.project_id)
            else {
                return Err(UserServiceError::UpdateProjectUser(format!(
                    "user {} is not a member of project {}",
                    input.user_id.as_str(),
                    input.project_id.as_str()
                )));
            };
            if let Some(v) = input.is_owner {
                up.is_owner = v;
            }
            if let Some(v) = input.scopes {
                up.scopes = Some(v);
            }
            Ok(up.clone())
        }
    }

    fn schema_with(store: &InMemoryUserService) -> AdminSchema {
        let query: Arc<dyn UserQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn UserMutationServices> = Arc::new(store.clone());
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(conduit_auth::Principal::system());
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(context)
            .finish()
    }

    fn schema_with_role_query(store: &InMemoryUserService, role: crate::role::Role) -> AdminSchema {
        let query: Arc<dyn UserQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn UserMutationServices> = Arc::new(store.clone());
        let role_query: Arc<dyn crate::role::RoleQueryServices> =
            Arc::new(FixedRoleQueryService { role });
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(role_query)
            .data({
                let mut context = conduit_auth::RequestContext::new();
                let _ = context.set_principal(conduit_auth::Principal::system());
                context
            })
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    /// A per-request context carrying a non-owner user principal with the given
    /// system scopes — used to drive the P-31 escalation guard.
    fn non_owner_ctx(scopes: &[&str]) -> conduit_auth::request_context::RequestContext {
        let mut principal = conduit_auth::Principal::user("2");
        for scope in scopes {
            principal = principal.with_scope(*scope);
        }
        let mut rc = conduit_auth::request_context::RequestContext::new();
        let _ = rc.set_principal(principal);
        rc
    }

    fn project_ctx(project_id: &str) -> conduit_auth::request_context::RequestContext {
        let principal = conduit_auth::Principal::user("actor")
            .with_scope(conduit_auth::scopes::Scope::project_role(
                project_id,
                conduit_auth::scopes::slug::WRITE_USERS,
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

    fn project_owner_ctx(project_id: &str) -> conduit_auth::request_context::RequestContext {
        let principal = conduit_auth::Principal::user("project-owner").with_scope(
            conduit_auth::scopes::Scope::project_membership(
                project_id,
                conduit_auth::scopes::slug::WILDCARD,
            ),
        );
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(principal);
        let _ = context.set_project_id(project_id);
        context
    }

    // -----------------------------------------------------------------
    // P-31 — privilege-escalation guard (guard_scope_grant)
    // -----------------------------------------------------------------

    /// A non-owner caller cannot self-promote / grant owner via `isOwner: true`.
    #[tokio::test]
    async fn update_user_denies_non_owner_setting_is_owner() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@x.io"));
        let schema = schema_with(&store);
        let query = r#"mutation { updateUser(id: "1", input: { isOwner: true }) { id } }"#;
        let resp = schema
            .execute(async_graphql::Request::new(query).data(non_owner_ctx(&[])))
            .await;
        assert!(!resp.errors.is_empty(), "non-owner isOwner must be denied");
        assert!(
            resp.errors[0].message.contains("owner"),
            "error should mention owner: {}",
            resp.errors[0].message
        );
        Ok(())
    }

    /// A non-owner caller cannot grant a scope they do not themselves hold.
    #[tokio::test]
    async fn update_user_denies_granting_unheld_scope() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@x.io"));
        let schema = schema_with(&store);
        // Caller holds only read_users; tries to grant write_settings.
        let query =
            r#"mutation { updateUser(id: "1", input: { scopes: ["write_settings"] }) { id } }"#;
        let resp = schema
            .execute(async_graphql::Request::new(query).data(non_owner_ctx(&["read_users"])))
            .await;
        assert!(
            !resp.errors.is_empty(),
            "granting an unheld scope must be denied"
        );
        Ok(())
    }

    /// A non-owner MAY grant a scope they DO hold (the grant ceiling is their
    /// own scope set).
    #[tokio::test]
    async fn update_user_allows_granting_held_scope() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@x.io"));
        let schema = schema_with(&store);
        let query =
            r#"mutation { updateUser(id: "1", input: { scopes: ["write_users"] }) { id } }"#;
        let resp = schema
            .execute(async_graphql::Request::new(query).data(non_owner_ctx(&["write_users"])))
            .await;
        assert!(
            resp.errors.is_empty(),
            "held scope grant allowed: {:?}",
            resp.errors
        );
        Ok(())
    }

    #[tokio::test]
    async fn role_id_grant_cannot_bypass_the_callers_scope_ceiling() {
        let store = InMemoryUserService::default();
        let schema = schema_with_role_query(
            &store,
            crate::role::Role {
                id: ID::from("9"),
                created_at: epoch(),
                updated_at: epoch(),
                name: "Privileged".to_owned(),
                level: crate::role::RoleLevel::System,
                project_id: None,
                scopes: Some(vec!["write_settings".to_owned()]),
            },
        );
        let mutation = r#"mutation {
            createUser(input: {
                email: "role-target@example.com", password: "secret", roleIDs: ["9"]
            }) { id }
        }"#;
        let response = schema
            .execute(
                async_graphql::Request::new(mutation)
                    .data(non_owner_ctx(&[conduit_auth::scopes::slug::WRITE_USERS])),
            )
            .await;

        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("write_settings"));
        assert!(lock(&store.users).is_empty());
    }

    /// No principal in context (crate's bare-schema tests) → guard is a no-op,
    /// so escalation fields pass through (the production extension is the gate).
    #[tokio::test]
    async fn update_user_guard_is_noop_without_principal() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@x.io"));
        let schema = schema_with(&store);
        let query = r#"mutation { updateUser(id: "1", input: { isOwner: true }) { id } }"#;
        let resp = schema.execute(async_graphql::Request::new(query)).await;
        assert!(
            resp.errors.is_empty(),
            "no-principal skip: {:?}",
            resp.errors
        );
        Ok(())
    }

    // -----------------------------------------------------------------
    // SDL parity
    // -----------------------------------------------------------------

    #[test]
    fn sdl_user_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type User",
            "type User",
            &[
                "projects(…): ProjectConnection!",
                "apiKeys(…): APIKeyConnection!",
                "channelOverrideTemplates(…): ChannelOverrideTemplateConnection!",
                "oidcIdentities(…): OIDCIdentityConnection!",
                "projectUsers: [UserProject!]",
                "userRoles: [UserRole!]",
            ],
        )?;
        assert!(sdl.contains("type User implements Node {"));
        assert!(snapshot.contains("type User implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_user_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type UserConnection",
            "type UserConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type UserEdge", "type UserEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_user_project_and_user_role_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        // UserProject: `user` and `project` edge fields are pending.
        assert_block_parity(
            &sdl,
            &snapshot,
            "type UserProject",
            "type UserProject",
            &["project: Project!"],
        )?;
        // UserRole: `user` and `role` edge fields are pending.
        assert_block_parity(
            &sdl,
            &snapshot,
            "type UserRole",
            "type UserRole",
            &["user: User!", "role: Role!"],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_user_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in ["enum UserStatus", "enum UserOrderField"] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_user_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input CreateUserInput",
            "input UpdateUserInput",
            "input UserOrder",
            "input AddUserToProjectInput",
            "input UpdateProjectUserInput",
            "input RemoveUserFromProjectInput",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        assert!(sdl.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    #[test]
    fn sdl_user_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input UserWhereInput",
            "input UserWhereInput",
            &[
                "hasProjectsWith: [ProjectWhereInput!]",
                "hasAPIKeysWith: [APIKeyWhereInput!]",
                "hasRolesWith: [RoleWhereInput!]",
                "hasChannelOverrideTemplatesWith: [ChannelOverrideTemplateWhereInput!]",
                "hasOidcIdentitiesWith: [OIDCIdentityWhereInput!]",
                "hasProjectUsersWith: [UserProjectWhereInput!]",
                "hasUserRolesWith: [UserRoleWhereInput!]",
            ],
        )
    }

    #[test]
    fn sdl_users_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query.users signature (snapshot type Query, lines 5713-5742).
        assert!(
            sdl.contains(
                "users(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: UserOrder, where: UserWhereInput): UserConnection!"
            ),
            "generated SDL missing the users connection signature: {sdl}"
        );

        // Mutations (snapshot type Mutation, lines 823-826 + 839-841).
        for signature in [
            "createUser(input: CreateUserInput!): User!",
            "updateUser(id: ID!, input: UpdateUserInput!): User!",
            "updateUserStatus(id: ID!, status: UserStatus!): User!",
            "deleteUser(id: ID!): Boolean!",
            "addUserToProject(input: AddUserToProjectInput!): UserProject!",
            "removeUserFromProject(input: RemoveUserFromProjectInput!): Boolean!",
            "updateProjectUser(input: UpdateProjectUserInput!): UserProject!",
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
    fn resolve_user_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_user_order(Some(UserOrder {
            direction: OrderDirection::Desc,
            field: UserOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(UserOrderSelection {
                direction: OrderDirection::Desc,
                term: UserOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_user_order_maps_updated_at_one_to_one() {
        let selection = resolve_user_order(Some(UserOrder {
            direction: OrderDirection::Asc,
            field: UserOrderField::UpdatedAt,
        }));
        assert_eq!(
            selection,
            Some(UserOrderSelection {
                direction: OrderDirection::Asc,
                term: UserOrderTerm::UpdatedAt,
            })
        );
        assert_eq!(resolve_user_order(None), None);
    }

    // -----------------------------------------------------------------
    // Resolver: createUser
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn create_user_with_defaults() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createUser(input: {
                        email: "a@b.c",
                        password: "secret",
                        firstName: "Ada"
                    }) {
                        id email status firstName isOwner
                        roles { edges { node { id name } } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let user = &data["createUser"];
        assert_eq!(user["id"], "1");
        assert_eq!(user["email"], "a@b.c");
        // ent default: status = activated.
        assert_eq!(user["status"], "activated");
        assert_eq!(user["firstName"], "Ada");
        assert_eq!(user["isOwner"], false);
        assert_eq!(user["roles"]["edges"], serde_json::json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn create_user_duplicate_email_surfaces_error() {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "dup@b.c"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createUser(input: { email: "dup@b.c", password: "x" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("email 'dup@b.c' already exists"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: updateUser / updateUserStatus
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_user_applies_partial_merge() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(3, "u@b.c"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateUser(id: "3", input: { firstName: "Renamed" }) {
                        id firstName email
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateUser"]["firstName"], "Renamed");
        assert_eq!(data["updateUser"]["email"], "u@b.c");
        Ok(())
    }

    #[tokio::test]
    async fn update_user_status_sets_status() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "u@b.c"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateUserStatus(id: "1", status: deactivated) { id status } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateUserStatus"]["status"], "deactivated");
        Ok(())
    }

    #[tokio::test]
    async fn update_user_missing_id_surfaces_wrapped_error() {
        let store = InMemoryUserService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateUser(id: "404", input: { firstName: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update user"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: deleteUser
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn delete_user_returns_true_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(7, "v@b.c"));
        let schema = schema_with(&store);

        let resp = schema.execute(r#"mutation { deleteUser(id: "7") }"#).await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteUser"], true);
        assert!(lock(&store.users).is_empty());
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: addUserToProject / removeUserFromProject / updateProjectUser
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn add_user_to_project_creates_membership() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(2, "member@example.com"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    addUserToProject(input: {
                        projectId: "5",
                        userId: "2",
                        isOwner: true,
                        scopes: ["read_channels"]
                    }) {
                        id userID projectID isOwner scopes
                        user { id email }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let up = &data["addUserToProject"];
        assert_eq!(up["userID"], "2");
        assert_eq!(up["projectID"], "5");
        assert_eq!(up["isOwner"], true);
        assert_eq!(up["scopes"][0], "read_channels");
        assert_eq!(up["user"]["email"], "member@example.com");
        Ok(())
    }

    #[tokio::test]
    async fn add_user_to_project_rejects_duplicate_membership() {
        let store = InMemoryUserService::default();
        lock(&store.user_projects).push(UserProject {
            id: ID::from("1"),
            created_at: epoch(),
            updated_at: epoch(),
            user_id: ID::from("2"),
            project_id: ID::from("5"),
            is_owner: false,
            scopes: None,
        });
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    addUserToProject(input: { projectId: "5", userId: "2" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("already a member of project"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn remove_user_from_project_returns_true_and_clears() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.user_projects).push(UserProject {
            id: ID::from("1"),
            created_at: epoch(),
            updated_at: epoch(),
            user_id: ID::from("2"),
            project_id: ID::from("5"),
            is_owner: false,
            scopes: None,
        });
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    removeUserFromProject(input: { projectId: "5", userId: "2" })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["removeUserFromProject"], true);
        assert!(lock(&store.user_projects).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn update_project_user_overwrites_owner_and_scopes() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.user_projects).push(UserProject {
            id: ID::from("1"),
            created_at: epoch(),
            updated_at: epoch(),
            user_id: ID::from("2"),
            project_id: ID::from("5"),
            is_owner: false,
            scopes: None,
        });
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateProjectUser(input: {
                        projectId: "5",
                        userId: "2",
                        isOwner: true,
                        scopes: ["write_channels"]
                    }) { isOwner scopes }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let up = &data["updateProjectUser"];
        assert_eq!(up["isOwner"], true);
        assert_eq!(up["scopes"][0], "write_channels");
        Ok(())
    }

    // -----------------------------------------------------------------
    // Resolver: users connection query
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn users_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@b.c"));
        lock(&store.users).push(sample_user(2, "b@b.c"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    users {
                        totalCount
                        edges { cursor node { id email status } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["users"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["email"], "a@b.c");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn users_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "a@b.c"));
        lock(&store.users).push(sample_user(2, "b@b.c"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    users(orderBy: { field: CREATED_AT, direction: DESC }) {
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
            Some(UserOrderSelection {
                direction: OrderDirection::Desc,
                term: UserOrderTerm::Id,
            })
        );
        let data = resp.data.into_json()?;
        assert_eq!(data["users"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn users_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ users { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("user service is not available"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn project_scoped_users_only_lists_authorized_project_members() -> Result<(), TestError> {
        let store = InMemoryUserService::default();
        lock(&store.users).extend([
            sample_user(1, "project-a@example.com"),
            sample_user(2, "project-b@example.com"),
        ]);
        lock(&store.user_projects).extend([
            UserProject {
                id: "1".into(),
                created_at: epoch(),
                updated_at: epoch(),
                user_id: "1".into(),
                project_id: "A".into(),
                is_owner: false,
                scopes: None,
            },
            UserProject {
                id: "2".into(),
                created_at: epoch(),
                updated_at: epoch(),
                user_id: "2".into(),
                project_id: "B".into(),
                is_owner: false,
                scopes: None,
            },
        ]);
        let response = schema_with(&store)
            .execute(
                async_graphql::Request::new("{ users { totalCount edges { node { email } } } }")
                    .data(project_ctx("A")),
            )
            .await;

        assert!(response.errors.is_empty(), "errors: {:?}", response.errors);
        let data = response.data.into_json()?;
        assert_eq!(data["users"]["totalCount"], 1);
        assert_eq!(
            data["users"]["edges"][0]["node"]["email"],
            "project-a@example.com"
        );
        Ok(())
    }

    #[tokio::test]
    async fn project_scoped_permission_rejects_global_user_mutations_and_project_b_membership() {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "target@example.com"));
        let schema = schema_with(&store);

        for mutation in [
            r#"mutation { updateUser(id: "1", input: { firstName: "changed" }) { id } }"#,
            r#"mutation { deleteUser(id: "1") }"#,
            r#"mutation { addUserToProject(input: { projectId: "B", userId: "1" }) { id } }"#,
        ] {
            let response = schema
                .execute(async_graphql::Request::new(mutation).data(project_ctx("A")))
                .await;
            assert!(
                !response.errors.is_empty(),
                "mutation unexpectedly succeeded"
            );
            assert!(
                response.errors[0].message.contains("permission denied"),
                "unexpected error: {}",
                response.errors[0].message
            );
        }
        assert_eq!(lock(&store.users)[0].first_name, "");
        assert!(lock(&store.user_projects).is_empty());
    }

    #[tokio::test]
    async fn only_selected_project_owner_can_promote_another_project_member() {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "target@example.com"));
        let schema = schema_with(&store);
        let mutation = r#"mutation {
            addUserToProject(input: { projectId: "A", userId: "1", isOwner: true }) { id }
        }"#;

        let denied = schema
            .execute(async_graphql::Request::new(mutation).data(project_ctx("A")))
            .await;
        assert_eq!(denied.errors.len(), 1);
        assert!(denied.errors[0].message.contains("owner"));

        let allowed = schema
            .execute(async_graphql::Request::new(mutation).data(project_owner_ctx("A")))
            .await;
        assert!(allowed.errors.is_empty(), "errors: {:?}", allowed.errors);
        assert!(lock(&store.user_projects)[0].is_owner);
    }

    #[tokio::test]
    async fn create_and_update_user_consume_all_previously_ignored_fields() {
        let store = InMemoryUserService::default();
        lock(&store.users).push(sample_user(1, "target@example.com"));
        let schema = schema_with(&store);

        let created = schema
            .execute(
                r#"mutation { createUser(input: {
                    email: "new@example.com", password: "secret",
                    status: deactivated, preferLanguage: "zh-CN",
                    avatar: "https://example.test/avatar.png", projectIDs: ["A"]
                }) { status preferLanguage avatar } }"#,
            )
            .await;
        assert!(created.errors.is_empty(), "errors: {:?}", created.errors);
        let created_json = created.data.into_json().expect("JSON response");
        assert_eq!(created_json["createUser"]["status"], "deactivated");
        assert_eq!(created_json["createUser"]["preferLanguage"], "zh-CN");
        assert_eq!(lock(&store.user_projects).len(), 1);

        let updated = schema
            .execute(
                r#"mutation { updateUser(id: "1", input: {
                    status: deactivated, preferLanguage: "fr",
                    addProjectIDs: ["B"]
                }) { status preferLanguage } }"#,
            )
            .await;
        assert!(updated.errors.is_empty(), "errors: {:?}", updated.errors);
        assert!(lock(&store.user_projects).iter().any(|membership| {
            membership.user_id.as_str() == "1" && membership.project_id.as_str() == "B"
        }));
    }
}

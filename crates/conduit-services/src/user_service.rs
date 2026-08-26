//! UserService — pure-logic scope aggregation + status/owner/language-preference
//! helpers, mirroring `conduit/internal/server/biz/user.go::ConvertUserToUserInfo`
//! and friends. No I/O, no DB, no JWT; this module is intentionally unit-testable.
//!
//! Only the testable "pure" subset of `biz/user.go` is ported here. The Go source
//! remains the canonical contract; tests below mirror the golden intent of the
//! Go `user_test.go` cases (`TestConvertUserToUserInfo_*`) without synthesizing
//! any snapshot — the input/output shape is reconstructed from the Go code.

use std::collections::BTreeSet;

use conduit_auth::ScopeSet;
use conduit_core::objects::user::{OidcIdentityInfo, RoleInfo, UserInfo, UserProjectInfo};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Placeholder password stored for OIDC-only users. Mirrors the Go constant
/// `conduit/internal/server/biz/auth.go::OIDC_ONLY_PLACEHOLDER`.
pub const OIDC_ONLY_PLACEHOLDER: &str = "!OIDC_SSO_ONLY!";

pub type UserServiceResult<T> = Result<T, UserServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UserServiceError {
    /// Mirrors the panic path in Go `ConvertUserToUserInfo(ctx, nil)`. Kept as
    /// an explicit variant so callers that want the panic-equivalent behavior
    /// can propagate it; the pure helpers below never construct it because
    /// Rust references are already non-null.
    #[error("user view is required (Go panics on nil)")]
    NilUserView,
    #[error("cannot delete owner user, transfer ownership first")]
    CannotDeleteOwner,
    /// Mirrors Go `biz/user.go::UpdateProjectUser` error at L479:
    ///   `fmt.Errorf("failed to find user project relationship: %w", err)`
    /// Pure guard: when the (user_id, project_id) row does not exist, Go
    /// surfaces this message; the Rust plan returns this variant so a
    /// repo-backed executor can propagate it. Asserted by Go
    /// `TestUpdateProjectUser_NotFound` (user_test.go:1038-1072).
    #[error("failed to find user project relationship")]
    UserProjectRelationshipNotFound,
    /// Mirrors Go `biz/user.go` permission gates at L87/L92/L101/L449/L456/
    /// L465/L535 — every `s.permissionValidator.Can*` failure is wrapped as
    /// `fmt.Errorf("permission denied: %w", err)`. The pure plan surfaces
    /// this variant when the caller reports the principal lacks authority;
    /// the underlying reason is opaque (Go wraps the validator's own error).
    /// Asserted by Go `TestUpdateProjectUser_UpdateIsOwner_PermissionDenied`
    /// (user_test.go:1415-1475).
    #[error("permission denied")]
    PermissionDenied,
}

// ---------------------------------------------------------------------------
// Role level enum — mirrors Go `conduit/internal/ent/role.LevelSystem|LevelProject`.
// ---------------------------------------------------------------------------

/// Mirrors Go `role.Level` enum. Go source (`ent/role/role.go:121-122`):
///   LevelSystem  Level = "system"
///   LevelProject Level = "project"
/// `IsSystemRole()` (`ent/extra.go:8-10`) treats `ProjectID == nil || *ProjectID == 0`
/// as system. We capture both the level enum and the explicit project-id rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoleLevel {
    #[default]
    System,
    Project,
}

/// A role as seen by the user-service aggregation. Mirrors the subset of Go
/// `ent.Role` fields consumed by `ConvertUserToUserInfo`:
///   - `Name`, `Scopes []string`, `Level role.Level`, `ProjectID *int`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoleView {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub level: RoleLevel,
    /// `None` => system role (mirrors Go `*int == nil`). `Some(0)` also counts
    /// as system per `IsSystemRole()` (`*r.ProjectID == 0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
}

impl RoleView {
    /// Mirrors Go `(*Role).IsSystemRole()` exactly: system iff project_id is
    /// absent OR equals 0.
    pub fn is_system_role(&self) -> bool {
        match self.project_id {
            None => true,
            Some(pid) => pid == 0,
        }
    }
}

// ---------------------------------------------------------------------------
// User status enum — mirrors Go `user.StatusActivated|StatusDeactivated`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Activated,
    #[default]
    Deactivated,
}

impl UserStatus {
    /// Mirrors Go `user.StatusActivated` semantics: only `Activated` users can
    /// authenticate / act.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Activated)
    }
}

// ---------------------------------------------------------------------------
// UserProject + UserView — mirrors the edges of `ent.User` read by
// `ConvertUserToUserInfo` (`Edges.Roles`, `Edges.ProjectUsers`,
// `Edges.OidcIdentities`).
// ---------------------------------------------------------------------------

/// One row of Go `ent.UserProject` as consumed by the aggregator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserProjectView {
    pub project_id: i64,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

/// One row of Go `ent.OIDCIdentity`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OidcIdentityView {
    pub id: i64,
    pub idp_name: String,
    pub issuer: String,
    pub subject: String,
    pub email: String,
}

/// A read-model of `ent.User` carrying exactly the fields
/// `ConvertUserToUserInfo` consumes. The owner flag is hoisted here
/// (Go reads `u.IsOwner`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UserView {
    pub id: i64,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub is_owner: bool,
    pub prefer_language: String,
    /// Empty string => no avatar (Go uses an empty-string default at the
    /// `ent.User` level and wraps the field as `*string` in `objects.UserInfo`).
    pub avatar: String,
    pub status: UserStatus,
    /// Raw password column (Go `u.Password`); used to derive `has_password`.
    pub password: String,
    /// Direct user scopes (Go `u.Scopes`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// All roles attached to the user, both system and project-scoped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<RoleView>,
    /// Project memberships (Go `Edges.ProjectUsers`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_users: Vec<UserProjectView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oidc_identities: Vec<OidcIdentityView>,
}

// ---------------------------------------------------------------------------
// Aggregation output — splits the four precedence layers so callers (RBAC)
// can re-use them. Mirrors the precedence order documented in CLAUDE.md
// (owner -> direct -> system-role -> project-membership -> project-role).
// ---------------------------------------------------------------------------

/// Effective scope layers computed from a `UserView`. Each layer is a
/// `BTreeSet<String>` (mirrors Go's `map[string]bool` accumulator, sorted for
/// deterministic output) plus a lazily-built `ScopeSet` view for RBAC
/// consumers (`conduit-auth::rbac::has_scope`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AggregatedScopes {
    /// Direct scopes from `u.Scopes`.
    pub direct: BTreeSet<String>,
    /// Scopes contributed by system-level roles (Go: roles where
    /// `IsSystemRole()` is true).
    pub system_role: BTreeSet<String>,
    /// Per-project membership scopes keyed by project id (Go:
    /// `UserProject.Scopes`).
    pub project_membership: Vec<(i64, BTreeSet<String>)>,
    /// Per-project role scopes, keyed by project id, derived from project
    /// roles attached to the user (Go: roles where `IsSystemRole()` is false).
    pub project_role: Vec<(i64, BTreeSet<String>)>,
}

impl AggregatedScopes {
    /// Flatten the four layers into a single deduplicated set, mirroring Go's
    /// `allScopes := map[string]bool` accumulation order in
    /// `ConvertUserToUserInfo` (direct first, then system-role, then — per
    /// project — membership then role).
    ///
    /// NOTE: Go only puts the *global* scopes (direct + system-role) into
    /// `UserInfo.Scopes`; project-scoped layers live under each
    /// `UserProjectInfo.Scopes`. This helper is provided for RBAC consumers
    /// that need the union; the conversion function below uses the per-layer
    /// views to populate `UserInfo` correctly.
    pub fn flattened(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        out.extend(self.direct.iter().cloned());
        out.extend(self.system_role.iter().cloned());
        for (_, set) in &self.project_membership {
            out.extend(set.iter().cloned());
        }
        for (_, set) in &self.project_role {
            out.extend(set.iter().cloned());
        }
        out
    }

    /// Build an `conduit-auth::ScopeSet` over the flattened union, for
    /// downstream RBAC checks (`conduit-auth::rbac::has_scope`).
    pub fn to_scope_set(&self) -> ScopeSet {
        ScopeSet::from(&self.flattened())
    }

    /// Build an `conduit-auth::ScopeSet` over the global layers only
    /// (direct + system-role), mirroring what Go assigns to `UserInfo.Scopes`.
    pub fn to_global_scope_set(&self) -> ScopeSet {
        let mut global = BTreeSet::new();
        global.extend(self.direct.iter().cloned());
        global.extend(self.system_role.iter().cloned());
        ScopeSet::from(&global)
    }

    /// Project-role scopes for one project (RBAC lookup).
    pub fn project_role_scopes(&self, project_id: i64) -> Option<&BTreeSet<String>> {
        self.project_role
            .iter()
            .find(|(pid, _)| *pid == project_id)
            .map(|(_, set)| set)
    }

    /// Project-membership scopes for one project (RBAC lookup).
    pub fn project_membership_scopes(&self, project_id: i64) -> Option<&BTreeSet<String>> {
        self.project_membership
            .iter()
            .find(|(pid, _)| *pid == project_id)
            .map(|(_, set)| set)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — the testable subset (task S10).
// ---------------------------------------------------------------------------

/// `is_active(user)` — mirrors the gate used by `authenticate_user` /
/// `authenticate_jwt` in `auth_service.rs` (Go: only `user.StatusActivated`
/// can authenticate). Kept here so user-status logic lives with the user
/// service per task S10.
pub fn is_active(user: &UserView) -> bool {
    user.status.is_active()
}

/// Owner promotion: an owner bypasses project limits. Mirrors Go's
/// `is_owner` short-circuits in `biz` + `rbac.rs`
/// (`PermissionSource::Owner`). Pure helper for downstream authorization
/// decisions; does not mutate the user.
pub fn owner_bypasses_project_limits(user: &UserView) -> bool {
    user.is_owner
}

/// Mirrors the Go delete-owner guard in `biz/user.go::DeleteUser`:
///   if u.IsOwner { return fmt.Errorf("cannot delete owner user, transfer ownership first") }
pub fn can_delete_user(user: &UserView) -> UserServiceResult<()> {
    if user.is_owner {
        Err(UserServiceError::CannotDeleteOwner)
    } else {
        Ok(())
    }
}

/// Prefer-language read helper. Mirrors Go's use of `u.PreferLanguage` in
/// `ConvertUserToUserInfo` (a plain string field). Returns `None` when the
/// stored value is empty, so callers can apply a server default uniformly.
pub fn prefer_language(user: &UserView) -> Option<&str> {
    if user.prefer_language.is_empty() {
        None
    } else {
        Some(&user.prefer_language)
    }
}

/// `has_password(user)` — mirrors Go's `u.Password != OIDC_ONLY_PLACEHOLDER`
/// check inside `ConvertUserToUserInfo`. OIDC-only users carry the sentinel
/// placeholder and report `has_password = false`. An empty password column
/// is also treated as "no password" so a freshly-created half-populated row
/// does not falsely claim to have credentials.
pub fn has_password(user: &UserView) -> bool {
    !user.password.is_empty() && user.password != OIDC_ONLY_PLACEHOLDER
}

/// Aggregate the effective scope layers for a user view. Precedence order
/// (CLAUDE.md): owner -> direct -> system-role -> project-membership ->
/// project-role. The owner layer is *not* a scope set per se — owners
/// bypass RBAC at the principal level — so this function returns the four
/// scope-bearing layers; callers consult `owner_bypasses_project_limits`
/// separately.
///
/// Mirrors the Go accumulation in `ConvertUserToUserInfo`:
///   1. `allScopes[scope]=true` over `u.Scopes`             (direct)
///   2. for each role where `IsSystemRole()`: add `r.Scopes` (system-role)
///   3. project-scoped roles are partitioned by `*r.ProjectID` and become
///      the project-role layer (NOT added to global `allScopes` in Go).
///   4. project membership scopes (`UserProject.Scopes`) live per-project
///      and are likewise NOT added to global `allScopes` in Go.
pub fn aggregate_scopes(user: &UserView) -> AggregatedScopes {
    let mut direct = BTreeSet::new();
    for scope in &user.scopes {
        direct.insert(scope.clone());
    }

    let mut system_role = BTreeSet::new();
    let mut project_role: Vec<(i64, BTreeSet<String>)> = Vec::new();
    for role in &user.roles {
        if role.is_system_role() {
            for scope in &role.scopes {
                system_role.insert(scope.clone());
            }
        } else {
            // Go dereferences `*r.ProjectID`; is_system_role already returned
            // false, so project_id cannot be None here, but we defend with 0.
            let pid = role.project_id.unwrap_or(0);
            // Locate-or-insert (Vec is small: one entry per project id).
            let exists = project_role
                .iter()
                .any(|(existing_pid, _)| *existing_pid == pid);
            if !exists {
                project_role.push((pid, BTreeSet::new()));
            }
            if let Some((_, set)) = project_role
                .iter_mut()
                .find(|(existing_pid, _)| *existing_pid == pid)
            {
                for scope in &role.scopes {
                    set.insert(scope.clone());
                }
            }
        }
    }

    let mut project_membership: Vec<(i64, BTreeSet<String>)> = Vec::new();
    for up in &user.project_users {
        let mut set = BTreeSet::new();
        for scope in &up.scopes {
            set.insert(scope.clone());
        }
        project_membership.push((up.project_id, set));
    }

    AggregatedScopes {
        direct,
        system_role,
        project_membership,
        project_role,
    }
}

/// Convert a `UserView` to a `UserInfo` object. Mirrors Go
/// `ConvertUserToUserInfo(ctx, u)` line-for-line:
///   - global roles only in `UserInfo.Roles`
///   - global scopes (direct + system-role) in `UserInfo.Scopes`
///   - per-project roles/scopes under each `UserProjectInfo`
///   - OIDC identities copied verbatim
///   - `has_password` derived from the password column
///
/// `id` is rendered as a typed GUID string (`"user:{id}"`) since the canonical
/// `GUID` type isn't ported yet (matches the `Guid = String` alias in
/// `conduit-core::objects::user`).
///
/// Go panics on `u == nil`; Rust references are non-null, so this function
/// returns `Ok` for any `&UserView`. The `NilUserView` error variant is kept
/// on the type for explicit opt-in callers and for parity documentation.
pub fn convert_user_to_user_info(user: &UserView) -> UserServiceResult<UserInfo> {
    // Keep the NilUserView variant reachable so dead-code analysis doesn't
    // strip it; it documents the Go panic parity surface.
    let _ = UserServiceError::NilUserView;

    let scopes = aggregate_scopes(user);

    // Global roles only (Go: `if !r.IsSystemRole() { continue }`).
    let roles: Vec<RoleInfo> = user
        .roles
        .iter()
        .filter(|r| r.is_system_role())
        .map(|r| RoleInfo {
            name: r.name.clone(),
        })
        .collect();

    // Global scopes = direct + system-role (Go: `lo.Keys(allScopes)`).
    let mut global_scope_strings: BTreeSet<String> = BTreeSet::new();
    global_scope_strings.extend(scopes.direct.iter().cloned());
    global_scope_strings.extend(scopes.system_role.iter().cloned());
    let global_scopes: Vec<String> = global_scope_strings.into_iter().collect();

    // Build a per-project view: membership scopes + project-role names.
    let mut user_projects: Vec<UserProjectInfo> = Vec::new();
    for up in &user.project_users {
        let project_role_names: Vec<RoleInfo> = user
            .roles
            .iter()
            .filter(|r| !r.is_system_role() && r.project_id == Some(up.project_id))
            .map(|r| RoleInfo {
                name: r.name.clone(),
            })
            .collect();

        user_projects.push(UserProjectInfo {
            project_id: format!("project:{}", up.project_id),
            is_owner: up.is_owner,
            scopes: up.scopes.clone(),
            roles: project_role_names,
        });
    }

    let oidc_identities: Vec<OidcIdentityInfo> = user
        .oidc_identities
        .iter()
        .map(|id| OidcIdentityInfo {
            id: format!("oidc_identity:{}", id.id),
            idp_name: id.idp_name.clone(),
            issuer: id.issuer.clone(),
            subject: id.subject.clone(),
            email: id.email.clone(),
        })
        .collect();

    let avatar = if user.avatar.is_empty() {
        None
    } else {
        Some(user.avatar.clone())
    };

    Ok(UserInfo {
        id: format!("user:{}", user.id),
        email: user.email.clone(),
        first_name: user.first_name.clone(),
        last_name: user.last_name.clone(),
        is_owner: user.is_owner,
        prefer_language: user.prefer_language.clone(),
        avatar,
        scopes: global_scopes,
        roles,
        projects: user_projects,
        oidc_identities,
        has_password: has_password(user),
    })
}

// ---------------------------------------------------------------------------
// build_user_cache_key — Mendel-the-7th 2026-07-06.
// Pure port of Go `biz/user.go::buildUserCacheKey` (user.go:261-263):
//   func buildUserCacheKey(id int) string { return fmt.Sprintf("user:%d", id) }
// Used by GetUserByID (user.go:229), invalidateUserCache (user.go:267), and
// every CacheInvalidation test in user_test.go (L1165/L1203/L1243/L1289/L1339).
// The key shape is the pure contract; cache I/O lives in the repo-backed
// UserService (pending).
// ---------------------------------------------------------------------------

/// Build the cache key for a user row. Mirrors Go `buildUserCacheKey(id)`
/// (`conduit/internal/server/biz/user.go:261-263`) exactly: `"user:{id}"`.
/// Every CacheInvalidation test in `user_test.go` relies on this shape.
pub fn build_user_cache_key(id: i64) -> String {
    format!("user:{}", id)
}

// ---------------------------------------------------------------------------
// Pure cascade plans — Mendel-the-7th 2026-07-06.
// These mirror the established `delete_role_plan` / `soft_delete_project_plan`
// pattern (Mendel-the-6th): emit the ordered side-effect steps a future
// repo-backed UserService must execute, plus the pure guards that carry Go's
// exact error semantics. Each plan accepts already-queried facts (existence,
// rows-affected, role-id lists) as pure inputs so the planning logic is
// unit-testable without a database — the DB-backed integration scenarios in
// `user_test.go` (TestAddUserToProject_*, TestRemoveUserFromProject_*,
// TestUpdateProjectUser_*) map to these plans as follows:
//   * the pure plan proves the decision/cascade shape is correct,
//   * the DB test (pending repo port) proves the ent statements match.
// ---------------------------------------------------------------------------

/// One ordered side-effect step in an `add_user_to_project` plan.
///
/// Mirrors Go `biz/user.go::AddUserToProject` (user.go:367-405):
///   1. `client.UserProject.Create()` with nillable `isOwner` / `scopes`
///      (user.go:371-382) — Go's ent builder skips the column when the pointer
///      is nil, so schema defaults apply; `Some(v)` sets the value explicitly.
///   2. When `roleIDs` is non-empty, `user.Update().AddRoleIDs(roleIDs...)`
///      (user.go:389-399) — preceded by a `client.User.Get` we do not model
///      here (the executor owns the repo handle).
///   3. `s.invalidateUserCache(ctx, userID)` (user.go:402) — always last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddUserToProjectStep {
    CreateUserProject {
        user_id: i64,
        project_id: i64,
        is_owner: Option<bool>,
        scopes: Option<Vec<String>>,
    },
    AddUserRoleLinks {
        user_id: i64,
        role_ids: Vec<i64>,
    },
    InvalidateUserCache {
        user_id: i64,
    },
}

/// The ordered plan returned by [`add_user_to_project_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddUserToProjectPlan {
    pub steps: Vec<AddUserToProjectStep>,
}

/// Pure plan for Go `AddUserToProject` (user.go:367-405).
///
/// Accepts the inputs that are pure at planning time. The caller — a future
/// repo-backed `UserService` — supplies them verbatim from the request. The
/// plan emits:
///   * [`AddUserToProjectStep::CreateUserProject`] always,
///   * [`AddUserToProjectStep::AddUserRoleLinks`] only when `role_ids` is
///     `Some` and non-empty (mirrors Go `if len(roleIDs) > 0` at user.go:389),
///   * [`AddUserToProjectStep::InvalidateUserCache`] always (user.go:402).
///
/// **Parity note — duplicate detection:** Go does NOT pre-check for an
/// existing (user_id, project_id) row — the duplicate case
/// (`TestAddUserToProject_DuplicateRelationship`, user_test.go:551-584) is
/// surfaced by the DB's unique constraint and propagated as
/// `"failed to add user to project: %w"`. That error lives in the repo layer,
/// not here; the plan always succeeds at planning time.
pub fn add_user_to_project_plan(
    user_id: i64,
    project_id: i64,
    is_owner: Option<bool>,
    scopes: Option<Vec<String>>,
    role_ids: Option<Vec<i64>>,
) -> AddUserToProjectPlan {
    let mut steps = Vec::with_capacity(3);
    steps.push(AddUserToProjectStep::CreateUserProject {
        user_id,
        project_id,
        is_owner,
        scopes,
    });
    if let Some(ids) = role_ids.as_ref()
        && !ids.is_empty()
    {
        steps.push(AddUserToProjectStep::AddUserRoleLinks {
            user_id,
            role_ids: ids.clone(),
        });
    }
    steps.push(AddUserToProjectStep::InvalidateUserCache { user_id });
    AddUserToProjectPlan { steps }
}

/// One ordered side-effect step in a `remove_user_from_project` plan.
///
/// Mirrors Go `biz/user.go::RemoveUserFromProject` (user.go:408-444):
///   1. `client.UserProject.Delete().Where(...).Exec(ctx)` (user.go:412-418).
///   2. When rowsAffected > 0: query project-scoped role ids attached to the
///      user (user.go:424-429), then `RemoveRoleIDs(projectRoleIDs...)`
///      (user.go:434-437), then invalidate cache (user.go:441). When
///      rowsAffected == 0 the whole cascade is skipped — Go returns nil at
///      user.go:420-422, which is the behavior
///      `TestRemoveUserFromProject_NotFound` (user_test.go:631-659) asserts
///      (idempotent removal of a non-existent relationship).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveUserFromProjectStep {
    DeleteUserProject {
        user_id: i64,
        project_id: i64,
    },
    /// Mirrors Go user.go:434-437:
    ///   `client.User.UpdateOneID(userID).RemoveRoleIDs(projectRoleIDs...)`
    /// `role_ids` is the already-queried list of project-scoped role ids
    /// attached to this user (user.go:424-429); when empty, Go still calls
    /// `RemoveRoleIDs()` with no args, which ent treats as a no-op. The Rust
    /// executor should likewise skip the update when `role_ids` is empty.
    RemoveProjectRoleLinks {
        user_id: i64,
        role_ids: Vec<i64>,
    },
    InvalidateUserCache {
        user_id: i64,
    },
}

/// The ordered plan returned by [`remove_user_from_project_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveUserFromProjectPlan {
    pub steps: Vec<RemoveUserFromProjectStep>,
}

/// Pure plan for Go `RemoveUserFromProject` (user.go:408-444).
///
/// `rows_affected` mirrors the integer Go reads from the UserProject delete
/// (user.go:412-418) and tests at user.go:420. `project_role_ids` is the list
/// Go would obtain from
///   `client.Role.Query().Where(role.ProjectIDEQ, role.HasUsersWith(user.IDEQ)).IDs(ctx)`
/// (user.go:424-429) — the caller has already queried it (filtered to this
/// project_id).
///
/// When `rows_affected == 0`, the plan is empty — this is the idempotent
/// no-op path `TestRemoveUserFromProject_NotFound` (user_test.go:631-659)
/// asserts. Otherwise the cascade is: `DeleteUserProject` →
/// `RemoveProjectRoleLinks(role_ids)` → `InvalidateUserCache`.
///
/// **Parity note — cross-project isolation:** `TestRemoveUserFromProject_WithRoles`
/// (user_test.go:661-781) verifies that removing the user from project 1
/// strips project-1-scoped role links but leaves project-2-scoped and global
/// roles intact. The pure plan carries only the project-1 role ids (caller
/// already filtered by project_id), so by construction no other project's
/// roles are touched.
pub fn remove_user_from_project_plan(
    user_id: i64,
    project_id: i64,
    rows_affected: u64,
    project_role_ids: Vec<i64>,
) -> RemoveUserFromProjectPlan {
    if rows_affected == 0 {
        // Mirrors Go user.go:420-422 — silent no-op, NOT an error.
        return RemoveUserFromProjectPlan { steps: vec![] };
    }
    RemoveUserFromProjectPlan {
        steps: vec![
            RemoveUserFromProjectStep::DeleteUserProject {
                user_id,
                project_id,
            },
            RemoveUserFromProjectStep::RemoveProjectRoleLinks {
                user_id,
                role_ids: project_role_ids,
            },
            RemoveUserFromProjectStep::InvalidateUserCache { user_id },
        ],
    }
}

/// One ordered side-effect step in an `update_project_user` plan.
///
/// Mirrors Go `biz/user.go::UpdateProjectUser` (user.go:447-523):
///   1. Optional `UpdateUserProject` when any of `is_owner` / `scopes` is
///      `Some` (user.go:483-496).
///   2. Optional `AddRoleIDs` / `RemoveRoleIDs` when role lists are non-empty
///      (user.go:506-514).
///   3. `s.invalidateUserCache(ctx, userID)` always (user.go:520).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateProjectUserStep {
    /// Mirrors Go user.go:483-493. Emitted only when `is_owner` or `scopes`
    /// is `Some` (Go: pointer non-nil). `Some(false)` / `Some(vec![])` still
    /// emit — Go's `mut.SetIsOwner(false)` / `mut.SetScopes([]string{})` are
    /// explicit writes, distinct from nil (no-op).
    UpdateUserProject {
        user_id: i64,
        project_id: i64,
        is_owner: Option<bool>,
        scopes: Option<Vec<String>>,
    },
    AddUserRoleLinks {
        user_id: i64,
        role_ids: Vec<i64>,
    },
    RemoveUserRoleLinks {
        user_id: i64,
        role_ids: Vec<i64>,
    },
    InvalidateUserCache {
        user_id: i64,
    },
}

/// The ordered plan returned by [`update_project_user_plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateProjectUserPlan {
    pub steps: Vec<UpdateProjectUserStep>,
}

/// Pure plan for Go `UpdateProjectUser` (user.go:447-523).
///
/// Accepts `relationship_exists` (the boolean form of Go's
/// `client.UserProject.Query().Where(...).Only(ctx)` success at user.go:472-
/// 480) and the optional field updates. When the relationship is missing the
/// plan returns [`UserServiceError::UserProjectRelationshipNotFound`] — the
/// Rust analogue of Go's
///   `fmt.Errorf("failed to find user project relationship: %w", err)`
/// (user.go:479), which `TestUpdateProjectUser_NotFound` (user_test.go:1038-
/// 1072) asserts.
///
/// **Parity note — permission gates:** Go performs THREE permission checks
/// before the relationship lookup (user.go:448-467):
///
///   1. `CanEditUserPermissions(ctx, userID, &projectID)` (L449),
///   2. `CanGrantScopes(ctx, scopes, &projectID)` when `scopes != nil` (L455),
///   3. `CanEditRole(ctx, roleID, &projectID)` per add-role id (L463).
///
/// These live in `permissionValidator` and depend on the request principal +
/// rbac state; they are NOT pure from the plan's perspective. The caller (a
/// future repo-backed `UserService`) must run them BEFORE invoking this plan,
/// returning [`UserServiceError::PermissionDenied`] on failure. That mirrors
/// `TestUpdateProjectUser_UpdateIsOwner_PermissionDenied` (user_test.go:1415-
/// 1475): a non-owner principal attempting `is_owner = Some(true)` is rejected
/// at the `CanEditUserPermissions` gate.
///
/// Step emission:
///   * [`UpdateProjectUserStep::UpdateUserProject`] only when `is_owner` or
///     `scopes` is `Some` (mirrors Go `if isOwner != nil` / `if scopes != nil`
///     at user.go:485-491). When both are `None`, Go still calls
///     `.Update().Save(ctx)` as a no-op write; the plan omits the step to
///     keep the meaningful-side-effect surface visible.
///   * [`UpdateProjectUserStep::AddUserRoleLinks`] when `add_role_ids` is
///     non-empty (Go L506-508).
///   * [`UpdateProjectUserStep::RemoveUserRoleLinks`] when `remove_role_ids`
///     is non-empty (Go L510-512).
///   * [`UpdateProjectUserStep::InvalidateUserCache`] always (Go L520).
pub fn update_project_user_plan(
    user_id: i64,
    project_id: i64,
    relationship_exists: bool,
    is_owner: Option<bool>,
    scopes: Option<Vec<String>>,
    add_role_ids: Vec<i64>,
    remove_role_ids: Vec<i64>,
) -> UserServiceResult<UpdateProjectUserPlan> {
    if !relationship_exists {
        // Mirrors Go user.go:478-480.
        return Err(UserServiceError::UserProjectRelationshipNotFound);
    }

    let mut steps = Vec::with_capacity(4);

    if is_owner.is_some() || scopes.is_some() {
        steps.push(UpdateProjectUserStep::UpdateUserProject {
            user_id,
            project_id,
            is_owner,
            scopes,
        });
    }

    if !add_role_ids.is_empty() {
        steps.push(UpdateProjectUserStep::AddUserRoleLinks {
            user_id,
            role_ids: add_role_ids,
        });
    }
    if !remove_role_ids.is_empty() {
        steps.push(UpdateProjectUserStep::RemoveUserRoleLinks {
            user_id,
            role_ids: remove_role_ids,
        });
    }

    steps.push(UpdateProjectUserStep::InvalidateUserCache { user_id });
    Ok(UpdateProjectUserPlan { steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_auth::slug; // 仅测试用：READ_DASHBOARD 等 scope slug 常量

    // -- mirrors of the Go test scaffolding --------------------------------

    fn role_view(
        name: &str,
        scopes: &[&str],
        level: RoleLevel,
        project_id: Option<i64>,
    ) -> RoleView {
        RoleView {
            name: name.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            level,
            project_id,
        }
    }

    fn basic_user() -> UserView {
        UserView {
            id: 1,
            email: "test@example.com".to_string(),
            first_name: "John".to_string(),
            last_name: "Doe".to_string(),
            prefer_language: "en".to_string(),
            avatar: "https://example.com/avatar.jpg".to_string(),
            status: UserStatus::Activated,
            password: "hashed-password".to_string(),
            scopes: vec!["read_channels".to_string(), "write_channels".to_string()],
            ..Default::default()
        }
    }

    fn sorted_scopes(scopes: &[String]) -> Vec<String> {
        let mut v = scopes.to_vec();
        v.sort();
        v
    }

    // -- TestConvertUserToUserInfo_BasicUser -------------------------------

    #[test]
    fn convert_basic_user_populates_direct_scopes_only() -> UserServiceResult<()> {
        let user = basic_user();
        let info = convert_user_to_user_info(&user)?;

        assert_eq!(info.email, "test@example.com");
        assert_eq!(info.first_name, "John");
        assert_eq!(info.last_name, "Doe");
        assert_eq!(info.prefer_language, "en");
        assert!(!info.is_owner);
        assert_eq!(
            info.avatar.as_deref(),
            Some("https://example.com/avatar.jpg")
        );
        assert_eq!(
            sorted_scopes(&info.scopes),
            sorted_scopes(&["read_channels".to_string(), "write_channels".to_string()])
        );
        assert!(info.roles.is_empty());
        assert!(info.projects.is_empty());
        assert!(info.has_password);
        Ok(())
    }

    // -- TestConvertUserToUserInfo_WithGlobalRoles -------------------------

    #[test]
    fn convert_with_global_roles_unions_direct_and_role_scopes() -> UserServiceResult<()> {
        let mut user = basic_user();
        user.scopes = vec!["custom_scope".to_string()];
        user.roles = vec![
            role_view(
                "Administrator",
                &["manage_users", "manage_projects", "manage_channels"],
                RoleLevel::System,
                None,
            ),
            role_view("Viewer", &["read_channels"], RoleLevel::System, None),
        ];

        let info = convert_user_to_user_info(&user)?;

        assert_eq!(info.roles.len(), 2);
        let mut names: Vec<String> = info.roles.iter().map(|r| r.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["Administrator".to_string(), "Viewer".to_string()]
        );

        assert_eq!(
            sorted_scopes(&info.scopes),
            sorted_scopes(&[
                "custom_scope".to_string(),
                "manage_users".to_string(),
                "manage_projects".to_string(),
                "manage_channels".to_string(),
                "read_channels".to_string(),
            ])
        );
        Ok(())
    }

    // -- TestConvertUserToUserInfo_WithProjectRoles ------------------------

    #[test]
    fn convert_with_project_roles_keeps_global_scopes_empty() -> UserServiceResult<()> {
        let mut user = basic_user();
        user.scopes = vec![];
        user.avatar.clear();
        user.roles = vec![
            role_view(
                "Project Admin",
                &["manage_project_channels", "manage_project_users"],
                RoleLevel::Project,
                Some(10),
            ),
            role_view(
                "Project Member",
                &["read_project_channels"],
                RoleLevel::Project,
                Some(10),
            ),
        ];
        user.project_users = vec![UserProjectView {
            project_id: 10,
            is_owner: false,
            scopes: vec!["project_scope_1".to_string(), "project_scope_2".to_string()],
        }];

        let info = convert_user_to_user_info(&user)?;

        // All roles are project-scoped => no global roles, no global scopes.
        assert!(info.roles.is_empty());
        assert!(info.scopes.is_empty());

        // Project info carries membership scopes + project-role names.
        assert_eq!(info.projects.len(), 1);
        let proj = &info.projects[0];
        assert_eq!(proj.project_id, "project:10");
        assert!(!proj.is_owner);
        assert_eq!(
            sorted_scopes(&proj.scopes),
            sorted_scopes(&["project_scope_1".to_string(), "project_scope_2".to_string(),])
        );
        let mut role_names: Vec<String> = proj.roles.iter().map(|r| r.name.clone()).collect();
        role_names.sort();
        assert_eq!(
            role_names,
            vec!["Project Admin".to_string(), "Project Member".to_string()]
        );
        Ok(())
    }

    // -- TestConvertUserToUserInfo_MixedRoles ------------------------------

    #[test]
    fn convert_with_mixed_roles_partitions_global_and_project_layers() -> UserServiceResult<()> {
        let mut user = basic_user();
        user.is_owner = true;
        user.scopes = vec!["user_scope_1".to_string()];
        user.roles = vec![
            role_view(
                "Global Admin",
                &["global_scope_1", "global_scope_2"],
                RoleLevel::System,
                None,
            ),
            role_view(
                "Project Admin",
                &["project_scope_1"],
                RoleLevel::Project,
                Some(20),
            ),
        ];
        user.project_users = vec![UserProjectView {
            project_id: 20,
            is_owner: true,
            scopes: vec!["up_scope_1".to_string()],
        }];

        let info = convert_user_to_user_info(&user)?;

        assert_eq!(info.roles.len(), 1);
        assert_eq!(info.roles[0].name, "Global Admin");
        assert_eq!(
            sorted_scopes(&info.scopes),
            sorted_scopes(&[
                "user_scope_1".to_string(),
                "global_scope_1".to_string(),
                "global_scope_2".to_string(),
            ])
        );

        assert_eq!(info.projects.len(), 1);
        let proj = &info.projects[0];
        assert!(proj.is_owner);
        assert_eq!(proj.scopes, vec!["up_scope_1".to_string()]);
        assert_eq!(proj.roles.len(), 1);
        assert_eq!(proj.roles[0].name, "Project Admin");
        Ok(())
    }

    // -- TestConvertUserToUserInfo_MultipleProjects ------------------------

    #[test]
    fn convert_with_multiple_projects_emits_each_membership() -> UserServiceResult<()> {
        let mut user = basic_user();
        user.scopes = vec![];
        user.avatar.clear();
        user.prefer_language.clear();
        user.project_users = vec![
            UserProjectView {
                project_id: 1,
                is_owner: true,
                scopes: vec!["p1_scope".to_string()],
            },
            UserProjectView {
                project_id: 2,
                is_owner: false,
                scopes: vec!["p2_scope".to_string()],
            },
        ];

        let info = convert_user_to_user_info(&user)?;

        assert_eq!(info.projects.len(), 2);
        let mut ids: Vec<String> = info.projects.iter().map(|p| p.project_id.clone()).collect();
        ids.sort();
        assert_eq!(ids, vec!["project:1".to_string(), "project:2".to_string()]);
        Ok(())
    }

    // -- Pure-helper tests (status / owner / language / has_password) ------

    #[test]
    fn is_active_only_for_activated_status() {
        let mut user = basic_user();
        assert!(is_active(&user));
        user.status = UserStatus::Deactivated;
        assert!(!is_active(&user));
    }

    #[test]
    fn owner_bypasses_project_limits_reads_is_owner() {
        let mut user = basic_user();
        assert!(!owner_bypasses_project_limits(&user));
        user.is_owner = true;
        assert!(owner_bypasses_project_limits(&user));
    }

    #[test]
    fn can_delete_user_rejects_owner() {
        let mut user = basic_user();
        assert!(can_delete_user(&user).is_ok());
        user.is_owner = true;
        assert_eq!(
            can_delete_user(&user),
            Err(UserServiceError::CannotDeleteOwner)
        );
    }

    #[test]
    fn prefer_language_returns_none_when_empty() {
        let mut user = basic_user();
        assert_eq!(prefer_language(&user), Some("en"));
        user.prefer_language.clear();
        assert_eq!(prefer_language(&user), None);
    }

    #[test]
    fn has_password_false_for_oidc_only_placeholder() {
        let mut user = basic_user();
        assert!(has_password(&user));
        user.password = OIDC_ONLY_PLACEHOLDER.to_string();
        assert!(!has_password(&user));
        user.password.clear();
        assert!(!has_password(&user));
    }

    // -- aggregate_scopes precedence / partitioning ------------------------

    #[test]
    fn aggregate_scopes_partitions_system_and_project_role_layers() {
        let mut user = basic_user();
        user.scopes = vec![slug::READ_DASHBOARD.to_string()];
        user.roles = vec![
            role_view(
                "SysRole",
                &[slug::READ_USERS, slug::READ_ROLES],
                RoleLevel::System,
                None,
            ),
            role_view(
                "ProjRole-A",
                &[slug::READ_PROJECTS],
                RoleLevel::Project,
                Some(7),
            ),
            role_view(
                "ProjRole-B",
                &[slug::WRITE_PROJECTS],
                RoleLevel::Project,
                Some(7),
            ),
            role_view(
                "ProjRole-C",
                &[slug::READ_API_KEYS],
                RoleLevel::Project,
                Some(8),
            ),
        ];

        let agg = aggregate_scopes(&user);

        // Direct layer is just READ_DASHBOARD.
        assert!(agg.direct.contains(slug::READ_DASHBOARD));
        assert!(!agg.direct.contains(slug::READ_USERS));

        // System-role layer is the union of SysRole scopes.
        assert!(agg.system_role.contains(slug::READ_USERS));
        assert!(agg.system_role.contains(slug::READ_ROLES));
        assert!(!agg.system_role.contains(slug::READ_PROJECTS));

        // Project-role layers are partitioned per project id.
        let proj7 = agg.project_role_scopes(7);
        assert!(proj7.is_some());
        let empty_proj = BTreeSet::new();
        let proj7 = proj7.unwrap_or(&empty_proj);
        assert!(proj7.contains(slug::READ_PROJECTS));
        assert!(proj7.contains(slug::WRITE_PROJECTS));
        assert!(!proj7.contains(slug::READ_API_KEYS));

        let proj8 = agg.project_role_scopes(8);
        assert!(proj8.is_some());
        assert!(proj8.is_some_and(|s| s.contains(slug::READ_API_KEYS)));

        assert!(agg.project_role_scopes(999).is_none());
    }

    #[test]
    fn aggregate_scopes_membership_layer_is_per_project() {
        let mut user = basic_user();
        user.scopes = vec![];
        user.project_users = vec![
            UserProjectView {
                project_id: 1,
                is_owner: false,
                scopes: vec![slug::READ_PROJECTS.to_string()],
            },
            UserProjectView {
                project_id: 2,
                is_owner: false,
                scopes: vec![slug::WRITE_PROJECTS.to_string()],
            },
        ];

        let agg = aggregate_scopes(&user);

        assert_eq!(agg.project_membership.len(), 2);
        assert!(
            agg.project_membership_scopes(1)
                .is_some_and(|s| s.contains(slug::READ_PROJECTS))
        );
        assert!(
            agg.project_membership_scopes(2)
                .is_some_and(|s| s.contains(slug::WRITE_PROJECTS))
        );
        // Membership scopes do NOT leak into the direct layer.
        assert!(!agg.direct.contains(slug::READ_PROJECTS));
        assert!(!agg.direct.contains(slug::WRITE_PROJECTS));
    }

    #[test]
    fn flattened_union_covers_all_four_layers() {
        let mut user = basic_user();
        user.scopes = vec![slug::READ_DASHBOARD.to_string()];
        user.roles = vec![
            role_view("Sys", &[slug::READ_USERS], RoleLevel::System, None),
            role_view("Proj", &[slug::READ_PROJECTS], RoleLevel::Project, Some(1)),
        ];
        user.project_users = vec![UserProjectView {
            project_id: 1,
            is_owner: false,
            scopes: vec![slug::READ_API_KEYS.to_string()],
        }];

        let agg = aggregate_scopes(&user);
        let flat = agg.flattened();

        assert!(flat.contains(slug::READ_DASHBOARD));
        assert!(flat.contains(slug::READ_USERS));
        assert!(flat.contains(slug::READ_PROJECTS));
        assert!(flat.contains(slug::READ_API_KEYS));
    }

    #[test]
    fn to_scope_set_round_trips_through_conduit_auth() {
        let mut user = basic_user();
        user.scopes = vec![slug::READ_DASHBOARD.to_string()];
        user.roles = vec![role_view(
            "Sys",
            &[slug::READ_USERS],
            RoleLevel::System,
            None,
        )];

        let agg = aggregate_scopes(&user);

        // Global view: only direct + system-role.
        let global = agg.to_global_scope_set();
        assert!(global.contains(slug::READ_DASHBOARD));
        assert!(global.contains(slug::READ_USERS));
        assert!(!global.contains(slug::READ_PROJECTS));

        // Full view: union of all layers.
        let full = agg.to_scope_set();
        assert!(full.contains(slug::READ_DASHBOARD));
        assert!(full.contains(slug::READ_USERS));
    }

    // -- RoleView::is_system_role parity with Go (*int == nil || == 0) -----

    #[test]
    fn is_system_role_treats_absent_or_zero_project_id_as_system() {
        assert!(role_view("a", &[], RoleLevel::System, None).is_system_role());
        assert!(role_view("a", &[], RoleLevel::System, Some(0)).is_system_role());
        assert!(!role_view("a", &[], RoleLevel::Project, Some(7)).is_system_role());
    }

    // ====================================================================
    // Mendel-the-7th 2026-07-06 — user_test.go migration sub-slice.
    // The DB/ent/xcache-backed integration scenarios (TestAddUserToProject_*,
    // TestRemoveUserFromProject_*, TestUpdateProjectUser_*, and all five
    // CacheInvalidation tests) cannot run without a repo port; they are
    // documented here as pure-plan parity tests that prove the decision /
    // cascade shape. Each test references its Go counterpart by name + line.
    // Full DB integration is pending the repo-backed UserService port.
    // ====================================================================

    // -- build_user_cache_key parity (user.go:261-263) --------------------
    // Used by every CacheInvalidation test in user_test.go:
    //   TestUpdateUser_CacheInvalidation (L1165), TestUpdateUserStatus_...
    //   (L1203), TestAddUserToProject_... (L1243), TestRemoveUserFromProject_...
    //   (L1289), TestUpdateProjectUser_... (L1339). The cache I/O itself is
    //   DB-backed (pending); the key FORMAT is the pure contract.
    #[test]
    fn build_user_cache_key_mirrors_go_sprintf_user_id_format() {
        assert_eq!(build_user_cache_key(0), "user:0");
        assert_eq!(build_user_cache_key(1), "user:1");
        assert_eq!(build_user_cache_key(42), "user:42");
        assert_eq!(build_user_cache_key(1_000_000), "user:1000000");
    }

    // -- add_user_to_project_plan — mirrors Go user.go:367-405, tested by
    // user_test.go TestAddUserToProject_* (L410-584).

    #[test]
    fn add_user_to_project_plan_success_emits_create_then_invalidate() {
        // Mirrors Go TestAddUserToProject_Success (user_test.go:410-446):
        // isOwner=Some(false), scopes=Some(["read_project","write_project"]),
        // roleIDs=nil.
        let plan = add_user_to_project_plan(
            10,
            20,
            Some(false),
            Some(vec![
                "read_project".to_string(),
                "write_project".to_string(),
            ]),
            None,
        );
        assert_eq!(
            plan.steps,
            vec![
                AddUserToProjectStep::CreateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: Some(false),
                    scopes: Some(vec![
                        "read_project".to_string(),
                        "write_project".to_string(),
                    ]),
                },
                AddUserToProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    #[test]
    fn add_user_to_project_plan_with_roles_inserts_add_role_links_step() {
        // Mirrors Go TestAddUserToProject_WithRoles (user_test.go:448-513):
        // isOwner=Some(true), scopes=Some(["custom_scope"]),
        // roleIDs=[role1, role2]. The plan must emit AddUserRoleLinks
        // *between* CreateUserProject and InvalidateUserCache.
        let plan = add_user_to_project_plan(
            10,
            20,
            Some(true),
            Some(vec!["custom_scope".to_string()]),
            Some(vec![1, 2]),
        );
        assert_eq!(
            plan.steps,
            vec![
                AddUserToProjectStep::CreateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: Some(true),
                    scopes: Some(vec!["custom_scope".to_string()]),
                },
                AddUserToProjectStep::AddUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![1, 2],
                },
                AddUserToProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    #[test]
    fn add_user_to_project_plan_with_nil_owner_passes_none_through() {
        // Mirrors Go TestAddUserToProject_WithNilOwner (user_test.go:515-549):
        // isOwner=nil, scopes=nil, roleIDs=nil. Go's ent builder then skips
        // SetIsOwner / SetScopes and schema defaults (is_owner=false,
        // scopes=[]) apply at the DB layer. The plan propagates None so the
        // executor can mirror Go's nillable-pointer-skip semantics.
        let plan = add_user_to_project_plan(10, 20, None, None, None);
        assert_eq!(
            plan.steps,
            vec![
                AddUserToProjectStep::CreateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: None,
                    scopes: None,
                },
                AddUserToProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    #[test]
    fn add_user_to_project_plan_empty_role_ids_omits_add_step() {
        // Mirrors Go user.go:389 guard `if len(roleIDs) > 0`. An empty
        // Some(vec![]) must NOT emit AddUserRoleLinks — this is the Go
        // behavior even though the slice is non-nil.
        let plan = add_user_to_project_plan(10, 20, Some(false), None, Some(vec![]));
        assert_eq!(
            plan.steps,
            vec![
                AddUserToProjectStep::CreateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: Some(false),
                    scopes: None,
                },
                AddUserToProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    // Note: Go TestAddUserToProject_DuplicateRelationship (user_test.go:551-
    // 584) asserts a DB-layer unique-constraint error. The plan layer cannot
    // surface duplicates (Go itself does not pre-check at user.go:367-405);
    // the repo-backed UserService (pending) must propagate ent's constraint
    // violation as "failed to add user to project: %w". Tracked as a pending
    // DB test, not a pure-plan test.

    // -- remove_user_from_project_plan — mirrors Go user.go:408-444, tested
    // by user_test.go TestRemoveUserFromProject_* (L586-781).

    #[test]
    fn remove_user_from_project_plan_success_emits_delete_roles_invalidate() {
        // Mirrors Go TestRemoveUserFromProject_Success (user_test.go:586-629):
        // one relationship exists, after deletion rows_affected=1 and the
        // project has no project-scoped roles attached to the user.
        let plan = remove_user_from_project_plan(10, 20, 1, vec![]);
        assert_eq!(
            plan.steps,
            vec![
                RemoveUserFromProjectStep::DeleteUserProject {
                    user_id: 10,
                    project_id: 20,
                },
                RemoveUserFromProjectStep::RemoveProjectRoleLinks {
                    user_id: 10,
                    role_ids: vec![],
                },
                RemoveUserFromProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    #[test]
    fn remove_user_from_project_plan_not_found_is_silent_noop() {
        // Mirrors Go TestRemoveUserFromProject_NotFound (user_test.go:631-659):
        // removing a non-existent relationship is IDEMPOTENT — Go returns nil
        // at user.go:420-422 when rowsAffected == 0, NOT an error. The plan
        // mirrors this with zero steps.
        let plan = remove_user_from_project_plan(10, 20, 0, vec![]);
        assert!(plan.steps.is_empty());
    }

    #[test]
    fn remove_user_from_project_plan_with_roles_carries_only_filtered_ids() {
        // Mirrors Go TestRemoveUserFromProject_WithRoles (user_test.go:661-781):
        // removing the user from project 1 must strip project-1 role links but
        // leave project-2 and global roles intact. The caller has already
        // filtered `project_role_ids` to project 1's role ids (Go does this
        // via `role.ProjectIDEQ(projectID)` at user.go:425); the plan therefore
        // only carries those ids, and by construction cannot touch project 2
        // or global roles.
        let plan = remove_user_from_project_plan(10, 1, 1, vec![100]);
        assert_eq!(
            plan.steps,
            vec![
                RemoveUserFromProjectStep::DeleteUserProject {
                    user_id: 10,
                    project_id: 1,
                },
                RemoveUserFromProjectStep::RemoveProjectRoleLinks {
                    user_id: 10,
                    role_ids: vec![100],
                },
                RemoveUserFromProjectStep::InvalidateUserCache { user_id: 10 },
            ]
        );
    }

    // -- update_project_user_plan — mirrors Go user.go:447-523, tested by
    // user_test.go TestUpdateProjectUser_* (L783-1072, 1353-1475).

    #[test]
    fn update_project_user_plan_not_found_returns_relationship_error() {
        // Mirrors Go TestUpdateProjectUser_NotFound (user_test.go:1038-1072):
        // when the (user_id, project_id) relationship does not exist, Go
        // returns `fmt.Errorf("failed to find user project relationship: %w",
        // err)` (user.go:479). The plan surfaces this as
        // UserProjectRelationshipNotFound before emitting any step.
        let result = update_project_user_plan(
            10,
            20,
            false, // relationship does not exist
            None,
            Some(vec!["read_project".to_string()]),
            vec![],
            vec![],
        );
        assert_eq!(
            result,
            Err(UserServiceError::UserProjectRelationshipNotFound)
        );
    }

    #[test]
    fn update_project_user_plan_update_scopes_emits_update_step() {
        // Mirrors Go TestUpdateProjectUser_UpdateScopes (user_test.go:783-825):
        // scopes=Some(["read_project","write_project","delete_project"]),
        // isOwner=nil, addRoleIDs=nil, removeRoleIDs=nil.
        let new_scopes = vec![
            "read_project".to_string(),
            "write_project".to_string(),
            "delete_project".to_string(),
        ];
        let plan =
            update_project_user_plan(10, 20, true, None, Some(new_scopes.clone()), vec![], vec![])
                .map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::UpdateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: None,
                    scopes: Some(new_scopes),
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_add_roles_emits_add_step_only() {
        // Mirrors Go TestUpdateProjectUser_AddRoles (user_test.go:827-891):
        // isOwner=nil, scopes=nil, addRoleIDs=[r1,r2], removeRoleIDs=nil.
        // The UpdateUserProject step is OMITTED because both is_owner and
        // scopes are None — Go's `mut.SetIsOwner` / `mut.SetScopes` are
        // pointer-nil-skipped (user.go:485-491), making the UPDATE a no-op.
        let plan =
            update_project_user_plan(10, 20, true, None, None, vec![1, 2], vec![]).map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::AddUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![1, 2],
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_remove_roles_emits_remove_step_only() {
        // Mirrors Go TestUpdateProjectUser_RemoveRoles (user_test.go:893-958):
        // starting from roles [r1, r2], remove [r1]. Only RemoveUserRoleLinks
        // is emitted (plus the mandatory cache invalidation).
        let plan =
            update_project_user_plan(10, 20, true, None, None, vec![], vec![1]).map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::RemoveUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![1],
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_add_and_remove_roles_emits_both_in_order() {
        // Mirrors Go TestUpdateProjectUser_AddAndRemoveRoles
        // (user_test.go:960-1036): starting from [r1], add [r2,r3] and remove
        // [r1] in one call. Both AddUserRoleLinks and RemoveUserRoleLinks are
        // emitted; Go applies them in that order (user.go:506-512: add first,
        // then remove).
        let plan = update_project_user_plan(10, 20, true, None, None, vec![2, 3], vec![1])
            .map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::AddUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![2, 3],
                },
                UpdateProjectUserStep::RemoveUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![1],
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_update_scopes_and_roles_emits_all() {
        // Mirrors Go TestUpdateProjectUser_UpdateScopesAndRoles
        // (user_test.go:1074-1135): scopes=Some([...]), addRoleIDs=[r1].
        // Both UpdateUserProject and AddUserRoleLinks are emitted.
        let new_scopes = vec!["read_project".to_string(), "write_project".to_string()];
        let plan = update_project_user_plan(
            10,
            20,
            true,
            None,
            Some(new_scopes.clone()),
            vec![1],
            vec![],
        )
        .map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::UpdateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: None,
                    scopes: Some(new_scopes),
                },
                UpdateProjectUserStep::AddUserRoleLinks {
                    user_id: 10,
                    role_ids: vec![1],
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_update_is_owner_emits_explicit_write() {
        // Mirrors Go TestUpdateProjectUser_UpdateIsOwner_Success
        // (user_test.go:1353-1413): isOwner=Some(true), scopes=nil,
        // addRoleIDs=nil, removeRoleIDs=nil. Some(true) is an explicit write
        // (Go `mut.SetIsOwner(true)` at user.go:486) — distinct from nil
        // (skipped) and from Some(false) (explicit clear). The plan must emit
        // UpdateUserProject here even though scopes is None.
        let plan = update_project_user_plan(10, 20, true, Some(true), None, vec![], vec![])
            .map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![
                UpdateProjectUserStep::UpdateUserProject {
                    user_id: 10,
                    project_id: 20,
                    is_owner: Some(true),
                    scopes: None,
                },
                UpdateProjectUserStep::InvalidateUserCache { user_id: 10 },
            ])
        );
    }

    #[test]
    fn update_project_user_plan_permission_denied_surfaces_caller_layer_error() {
        // Mirrors Go TestUpdateProjectUser_UpdateIsOwner_PermissionDenied
        // (user_test.go:1415-1475): a non-owner principal attempting
        // is_owner=Some(true) is rejected by `CanEditUserPermissions`
        // (user.go:448-451) BEFORE the relationship lookup. The plan cannot
        // see the principal, so it documents the contract: PermissionDenied
        // is surfaced by the caller (the future repo-backed UserService),
        // not by the plan itself. We assert the variant exists and carries
        // Go's exact error prefix "permission denied" (user.go:450).
        assert_eq!(
            format!("{}", UserServiceError::PermissionDenied),
            "permission denied"
        );
        // Parity with Go: the wrapped validator error is opaque (Go
        // `fmt.Errorf("permission denied: %w", err)`); Rust mirrors this by
        // carrying no payload — callers attach context at the repo layer.
    }

    #[test]
    fn update_project_user_plan_all_none_emits_only_cache_invalidation() {
        // Edge case complementary to the Go suite: when isOwner, scopes,
        // addRoleIDs, and removeRoleIDs are ALL absent (None / empty), the
        // plan degenerates to a single cache-invalidation. Go still issues
        // a no-op UPDATE (user.go:483-496) + no-op user mutation; the plan
        // surfaces this as one InvalidateUserCache step so executors can
        // short-circuit the redundant writes.
        let plan =
            update_project_user_plan(10, 20, true, None, None, vec![], vec![]).map(|p| p.steps);
        assert_eq!(
            plan,
            Ok(vec![UpdateProjectUserStep::InvalidateUserCache {
                user_id: 10
            }])
        );
    }
}

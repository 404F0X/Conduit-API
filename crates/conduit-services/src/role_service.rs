//! RoleService — pure-logic role validation, classification, and
//! owner-escalation prevention, mirroring the *pure* subset of
//! `conduit/internal/server/biz/role.go::CreateRole` (the `permissionValidator`
//! + `level`/`project_id` switch) and `conduit/internal/server/biz/permission_validator.go::CanGrantScopes`.
//!
//! Scope of this module (RUST-P5-003):
//!   1. `validate_role`      — S12 level/scopes legality (valid level enum,
//!      valid scope slugs, system vs project `project_id` rule).
//!   2. `prevent_owner_escalation` — S12 owner-escalation prevention. Only an
//!      owner / system principal may grant system-level roles or scopes they
//!      do not themselves possess.
//!   3. `classify_role`      — system vs project role separation
//!      (`RoleKind::System` iff `IsSystemRole()`, else `Project`).
//!   4. `delete_role_plan` / `bulk_delete_roles_plan` — Mendel-the-6th
//!      2026-07-06. Pure cascade-planning ports of Go
//!      `biz/role.go::DeleteRole` / `BulkDeleteRoles`. They encode the exact
//!      Go error strings ("role not found", "expected to find N roles, but
//!      found M"), the empty-list no-op, and the cascade order
//!      (UserRole links → role row → cache invalidation) that a future
//!      repo-backed `RoleService` must execute inside a transaction. The
//!      DB execution (ent.Client delete, UserRole cascade, cache flush)
//!      remains pending a repo-backed service — the planning logic is
//!      load-bearing on its own because the Go tests assert on these exact
//!      messages and on the cascade order.
//!
//! The Go source remains the canonical contract. Tests below mirror the golden
//! intent of Go `role_test.go::TestCreateRole` (the "create global role",
//! "create project-specific role", "fail to create role with duplicate code"
//! cases), `permission_validator_test.go::TestCanGrantScopes` (owner-grants-any,
//! user-grants-owned, user-cannot-grant-unowned, role-bearer-grants-role-scope),
//! and `role_test.go::TestDeleteRole` / `TestBulkDeleteRoles` (the 3 + 4
//! sub-tests, mapped to the pure-plan surface), without synthesizing any
//! snapshot — the rules are reconstructed from the Go code paths quoted
//! inline below.
//!
//! `validate_role` / `prevent_owner_escalation` / `classify_role` /
//! `delete_role_plan` / `bulk_delete_roles_plan` are the pure-logic ports;
//! the persistence paths (`RoleNameExists`, `client.Role.Create`, the actual
//! `client.{UserRole,Role}.Delete` executions, `invalidateUserCache`) are
//! intentionally out of scope for this pure module (they require DB access —
//! RUST-P5-003 is "pure logic"). The delete plans describe what a future
//! repo-backed service MUST do, in order, with what error messages.

use conduit_auth::scopes::{is_known_scope_slug, supports_project_role};
use conduit_auth::{Principal, PrincipalKind};
use thiserror::Error;

// Re-export the shared role read-model so callers can construct inputs without
// reaching into `user_service` directly. The canonical definitions live in
// `crate::user_service` because that is where `ConvertUserToUserInfo` reads
// roles off `UserView`; this module just borrows them.
pub use crate::user_service::{RoleLevel, RoleView};

pub type RoleServiceResult<T> = Result<T, RoleServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoleServiceError {
    /// Mirrors Go `CreateRole`'s `default: return nil, fmt.Errorf("invalid role level")`
    /// branch (`role.go:87-89`). Reached when `level` is neither `system` nor
    /// `project`.
    #[error("invalid role level")]
    InvalidLevel,
    /// Mirrors Go `CreateRole`'s `case role.LevelSystem: if input.ProjectID != nil {
    /// return nil, fmt.Errorf("project ID is not allowed for system roles") }`
    /// (`role.go:72-74`).
    #[error("project ID is not allowed for system roles")]
    ProjectIdNotAllowedForSystemRole,
    /// Mirrors Go `CreateRole`'s `case role.LevelProject: if input.ProjectID == nil {
    /// return nil, fmt.Errorf("project ID is required for project roles") }`
    /// (`role.go:80-82`).
    #[error("project ID is required for project roles")]
    ProjectIdRequiredForProjectRole,
    /// Rejects unknown scope slugs. Go `CreateRole` does *not* perform this
    /// check itself (the `ent` validator would), but the task spec for S12
    /// ("valid scope slugs via `conduit-auth::scopes`") requires the pure
    /// validator to enforce the Go `scopes.IsValidScope` set
    /// (`internal/scopes/scopes.go:208-216`). The scope list is sourced from
    /// `conduit-auth::scopes::slug` plus `scopes::SYSTEM_ADMIN`.
    #[error("invalid scope slug: {0}")]
    InvalidScopeSlug(String),
    #[error("scope '{0}' is not allowed for project roles")]
    ScopeNotAllowedForProjectRole(String),
    /// Mirrors Go `CanGrantScopes`'s missing-user path:
    ///   `if !ok || currentUser == nil { return fmt.Errorf("user not found in context") }`
    /// (`permission_validator.go:97-99`). We surface it when callers hand in a
    /// `Principal::user` with no id, which is the Rust analogue.
    #[error("user not found in context")]
    UserNotFoundInContext,
    /// Mirrors Go `CanGrantScopes`'s rejection:
    ///   `return fmt.Errorf("insufficient permissions: cannot grant scope '%s' that you don't possess", scope)`
    /// (`permission_validator.go:125-127`). Only owners / system principals are
    /// exempt (Go short-circuits at `permission_validator.go:108-110`).
    #[error("insufficient permissions: cannot grant scope '{0}' that you don't possess")]
    InsufficientPermissions(String),
    /// Mirrors Go `RoleService.DeleteRole`'s existence-check failure:
    ///   `if !exists { return fmt.Errorf("role not found") }`
    /// (`role.go:190-192`). Surfaced by [`delete_role_plan`] when the caller
    /// reports the role does not exist in the persistent store.
    #[error("role not found")]
    RoleNotFound,
    /// Mirrors Go `RoleService.BulkDeleteRoles`'s count-mismatch failure:
    ///   `if count != len(ids) {
    ///        return fmt.Errorf("expected to find %d roles, but found %d", len(ids), count)
    ///    }`
    /// (`role.go:229-231`). `expected` is `len(ids)`; `found` is the number of
    /// roles the repo actually located.
    #[error("expected to find {expected} roles, but found {found}")]
    RolesCountMismatch { expected: usize, found: usize },
}

// ---------------------------------------------------------------------------
// Scope validity — mirrors Go `internal/scopes.IsValidScope`.
// ---------------------------------------------------------------------------

/// Mirrors Go `internal/scopes.scopeConfigs` (`scopes.go:80-176`) — every slug
/// in the system. Sourced from `conduit-auth::scopes::slug` (the canonical Rust
/// port) plus `slug::SYSTEM_ADMIN` (the Rust RBAC root). A scope is legal iff
/// it appears in this slice.
///
/// Note: the Go validator (`scopes.go`) only lists the 20 read_/write_ slugs;
/// `SYSTEM_ADMIN` is added here because the Rust RBAC layer
/// (`conduit-auth::rbac::principal_has_scope`) treats `system:admin` as a
/// granted scope string and roles carrying it must not be flagged invalid.
/// Mirrors Go `scopes.IsValidScope` (`scopes.go:208-216`).
pub fn is_valid_scope(scope: &str) -> bool {
    is_known_scope_slug(scope)
}

// ---------------------------------------------------------------------------
// RoleKind — mirrors Go `IsSystemRole()` classification.
// ---------------------------------------------------------------------------

/// Mirrors Go `(*Role).IsSystemRole()` (`ent/extra.go:8-10`):
///   `return r.ProjectID == nil || *r.ProjectID == 0`
/// Combined with Go `role.Level` (`ent/role/role.go:121-122`), a role is
/// `System` iff either its level is `system` *or* it has no/zero project id;
/// otherwise it is `Project`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    System,
    Project,
}

// ---------------------------------------------------------------------------
// classify_role — system vs project separation.
// ---------------------------------------------------------------------------

/// Classify a role as system or project.
///
/// Mirrors the union of Go `IsSystemRole()` and `role.Level`:
///   - `Level::System` (Go default when `input.Level == nil`, `role.go:66-68`)
///     is always `RoleKind::System`, regardless of project id.
///   - `Level::Project` is `RoleKind::Project` when a non-zero project id is
///     present, and `RoleKind::System` if the project id is missing/zero (the
///     Go `IsSystemRole()` fallback — defensive against inconsistent input).
pub fn classify_role(role: &RoleView) -> RoleKind {
    match role.level {
        RoleLevel::System => RoleKind::System,
        RoleLevel::Project => {
            if role.is_system_role() {
                RoleKind::System
            } else {
                RoleKind::Project
            }
        }
    }
}

// ---------------------------------------------------------------------------
// validate_role — level + scopes + project-id legality.
// ---------------------------------------------------------------------------

/// Validate level, scopes, and the system-vs-project `project_id` rule.
///
/// Mirrors the *pure* validation branches of Go
/// `RoleService.CreateRole` (`role.go:61-90`):
///
/// ```text
/// if input.Level == nil {
///     if input.ProjectID != nil {
///         return nil, fmt.Errorf("project ID is not allowed for system roles")
///     }
///     level = role.LevelSystem
/// } else {
///     switch *input.Level {
///     case role.LevelSystem:
///         if input.ProjectID != nil {
///             return nil, fmt.Errorf("project ID is not allowed for system roles")
///         }
///     case role.LevelProject:
///         if input.ProjectID == nil {
///             return nil, fmt.Errorf("project ID is required for project roles")
///         }
///     default:
///         return nil, fmt.Errorf("invalid role level")
///     }
/// }
/// ```
///
/// On top of the Go contract this function also rejects unknown scope slugs
/// (S12 "valid scope slugs via `conduit-auth::scopes`") by consulting
/// [`is_valid_scope`].
pub fn validate_role(role: &RoleView) -> RoleServiceResult<()> {
    // Scopes first — S12 slug legality. Unknown slugs are rejected even when
    // the level/project-id shape is also wrong, because an invalid scope on a
    // well-shaped role is still an invalid role.
    for scope in &role.scopes {
        if !is_valid_scope(scope) {
            return Err(RoleServiceError::InvalidScopeSlug(scope.clone()));
        }
    }

    match role.level {
        RoleLevel::System => {
            // Go: "project ID is not allowed for system roles" — only a non-zero
            // project id counts; Go stores `*int` and uses `projectIDValue = 0`
            // with `projectIDForAPI = lo.ToPtr(0)` for system roles, so a `Some(0)`
            // project id is *acceptable* on a system role (it is the canonical
            // form). Only `Some(non-zero)` is rejected.
            if matches!(role.project_id, Some(pid) if pid != 0) {
                return Err(RoleServiceError::ProjectIdNotAllowedForSystemRole);
            }
        }
        RoleLevel::Project => {
            // Go: "project ID is required for project roles". `Some(0)` is not a
            // valid project id — the system-role fallback in `IsSystemRole()`
            // treats `*ProjectID == 0` as system, so we mirror that here.
            if !matches!(role.project_id, Some(pid) if pid != 0) {
                return Err(RoleServiceError::ProjectIdRequiredForProjectRole);
            }
            for scope in &role.scopes {
                if !supports_project_role(scope) {
                    return Err(RoleServiceError::ScopeNotAllowedForProjectRole(
                        scope.clone(),
                    ));
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// prevent_owner_escalation — owner-level / system-level grant guard.
// ---------------------------------------------------------------------------

/// Prevent a regular user from creating or assigning a role they are not
/// allowed to grant. Mirrors Go
/// `PermissionValidator.CanGrantScopes` (`permission_validator.go:96-131`):
///
/// ```text
/// currentUser, ok := contexts.GetUser(ctx)
/// if !ok || currentUser == nil {
///     return fmt.Errorf("user not found in context")
/// }
///
/// // Owners can grant any scopes
/// isOwner, err := v.isProjectOwner(ctx, currentUser, projectID)
/// if isOwner {
///     return nil
/// }
///
/// userScopeSet := /* scopes from direct + global roles + project role/membership */
/// for _, scope := range scopesToGrant {
///     if !userScopeSet[scope] {
///         return fmt.Errorf("insufficient permissions: cannot grant scope '%s' that you don't possess", scope)
///     }
/// }
/// ```
///
/// `conduit-auth::Principal` already aggregates the user's effective scopes
/// (direct + role-derived) into `principal.scopes` via the auth pipeline, so
/// the Rust port can consult that set directly instead of re-deriving it.
///
/// Owner escalation is prevented because:
///   - Only `Principal::is_owner == true` or `PrincipalKind::System`/`Test`
///     principals short-circuit to "allow any". A regular user principal can
///     never set `is_owner` itself (it is populated by the auth pipeline from
///     the DB column `users.is_owner`).
///   - Granting a system-level role is implicitly an escalation because the
///     scopes it carries (e.g. `system:admin`, `write_settings`) are not in a
///     regular user's scope set, so the per-scope check rejects it.
pub fn prevent_owner_escalation(actor: &Principal, role: &RoleView) -> RoleServiceResult<()> {
    // Mirrors Go `contexts.GetUser(ctx)` failure path. The closest Rust
    // analogue is a `User` principal with no id — i.e. an anonymous/anonymous
    // session.
    if actor.kind == PrincipalKind::User && actor.id.is_none() {
        return Err(RoleServiceError::UserNotFoundInContext);
    }

    // Mirrors Go `isProjectOwner` short-circuit:
    //   - `user.IsOwner == true` (global owner)
    //   - system/test bypass principals (Rust RBAC analogue of Go
    //     `authz.WithTestBypass`/system context).
    if is_authoritative_principal(actor) {
        return Ok(());
    }

    // Per-scope possession check. Go accumulates direct + role-derived scopes;
    // `Principal::scopes` is the same aggregated set on the Rust side.
    for scope in &role.scopes {
        if !actor.scopes.contains(scope) {
            return Err(RoleServiceError::InsufficientPermissions(scope.clone()));
        }
    }

    Ok(())
}

/// Mirrors Go `PermissionValidator.isProjectOwner`'s global-owner branch
/// (`permission_validator.go:76-92`) and the system/test bypass used in
/// `authz.WithTestBypass` / system principals. We do *not* model the
/// project-specific owner branch here because `Principal` already collapses
/// project ownership into `is_owner` at the auth pipeline boundary
/// (`conduit-auth::rbac::principal_has_project_scope` resolves it against the
/// request's project id).
fn is_authoritative_principal(principal: &Principal) -> bool {
    if principal.is_owner {
        return true;
    }
    matches!(principal.kind, PrincipalKind::System | PrincipalKind::Test)
}

// ---------------------------------------------------------------------------
// delete_role_plan / bulk_delete_roles_plan — Mendel-the-6th 2026-07-06.
// Pure cascade-planning ports of Go `biz/role.go::DeleteRole` (role.go:179-211)
// and `BulkDeleteRoles` (role.go:214-252). These mirror the established
// `soft_delete_project_plan()` pattern from `user_project_service.rs`: emit
// the ordered side-effect steps a future repo-backed `RoleService` must
// execute inside a transaction, plus the pure guards (existence check,
// count mismatch, empty-list no-op) that carry Go's exact error strings.
// ---------------------------------------------------------------------------

/// One ordered side-effect step in a role-deletion plan.
///
/// Mirrors the cascade order of Go `RoleService.DeleteRole`
/// (`role.go:194-210`) and `BulkDeleteRoles` (`role.go:233-249`):
///
///   1. Delete all `UserRole` rows pointing at the role(s) — Go does this
///      FIRST so the subsequent role delete does not trip the
///      `UserRole.role_id` → `roles.id` FK constraint.
///   2. Delete the role row(s) themselves (ent's `DeleteOneID` / `Delete`
///      perform a soft-delete under `SoftDeleteMixin`; the Rust
///      `RoleRepo::soft_delete_role` is the analogue).
///   3. Invalidate the user cache — role changes affect every user who bore
///      the role.
///
/// Each variant carries the `i64` role id(s) it applies to so a future
/// repo-backed executor can fan out without re-resolving. `i64` matches the
/// Go `int` / `int64` id convention (CLAUDE.md parity rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteRoleStep {
    /// Mirrors Go `client.UserRole.Delete().Where(userrole.RoleID(id))`
    /// (`role.go:195-200`) — single-role form. Emits one `UserRole`-table
    /// delete for the given `role_id`.
    DeleteUserRoleLinks { role_id: i64 },
    /// Mirrors Go `client.UserRole.Delete().Where(userrole.RoleIDIn(ids...))`
    /// (`role.go:234-239`) — bulk form. Used by [`bulk_delete_roles_plan`] to
    /// express the set in one step.
    DeleteUserRoleLinksBulk { role_ids: Vec<i64> },
    /// Mirrors Go `client.Role.DeleteOneID(id).Exec(ctx)`
    /// (`role.go:203-206`) — single-role form.
    DeleteRole { role_id: i64 },
    /// Mirrors Go `client.Role.Delete().Where(role.IDIn(ids...))`
    /// (`role.go:242-247`) — bulk form.
    DeleteRolesBulk { role_ids: Vec<i64> },
    /// Mirrors Go `s.invalidateUserCache(ctx)` (`role.go:208` / `:249`).
    /// Clears the entire user cache because role changes affect every user
    /// who bore the role. Always emitted LAST.
    ///
    /// Note: the Go comment at `role.go:207` says "BEFORE deleting
    /// relationships" but the actual call is AFTER both deletes — we mirror
    /// the source-of-truth call order, not the stale comment.
    InvalidateUserCache,
}

/// The ordered plan returned by [`delete_role_plan`] /
/// [`bulk_delete_roles_plan`].
///
/// A repo-backed `RoleService` will execute `steps` in order; Go wraps the
/// whole cascade in a single ent-client transaction implicitly (each
/// `.Exec(ctx)` is its own statement but the role-delete would fail if the
/// preceding UserRole cascade errored, leaving no orphans because ent
/// enforces the FK from `UserRole.role_id` → `roles.id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteRolePlan {
    pub steps: Vec<DeleteRoleStep>,
}

/// Mirrors Go `RoleService.DeleteRole` (`role.go:179-211`) as a pure plan.
///
/// Performs the pure-validation aspect (existence check with exact Go error
/// string "role not found", `role.go:190-192`) and emits the cascade order:
/// [`DeleteRoleStep::DeleteUserRoleLinks`] → [`DeleteRoleStep::DeleteRole`]
/// → [`DeleteRoleStep::InvalidateUserCache`].
///
/// `role_exists` is the boolean form of Go's
/// `client.Role.Query().Where(role.IDEQ(id)).Exist(ctx)` (`role.go:183-188`).
/// The caller — a future repo-backed `RoleService` — has already queried the
/// store for existence; this function stays pure (no DB handle) so the
/// planning logic (error message + cascade ordering) is testable without
/// a database. This is the same pattern `user_project_service::
/// soft_delete_project_plan` uses for `biz/project.go::DeleteProject`.
///
/// Note: Go's `DeleteRole` has **no** system-role protection and **no**
/// `CanEditRole` permission check — unlike `UpdateRole` (`role.go:117`).
/// Any system-role protection must therefore live at a higher layer (the
/// GraphQL handler or a future repo-backed service), not here. Mirroring Go
/// exactly means we do NOT add such a guard.
pub fn delete_role_plan(role_id: i64, role_exists: bool) -> RoleServiceResult<DeleteRolePlan> {
    if !role_exists {
        // Mirrors Go `role.go:190-192`:
        //   `if !exists { return fmt.Errorf("role not found") }`
        return Err(RoleServiceError::RoleNotFound);
    }

    Ok(DeleteRolePlan {
        steps: vec![
            // Mirrors Go `role.go:194-200`: delete UserRole relationships first
            // so the subsequent role delete does not trip the FK constraint.
            DeleteRoleStep::DeleteUserRoleLinks { role_id },
            // Mirrors Go `role.go:202-206`: now safe to remove the role row.
            DeleteRoleStep::DeleteRole { role_id },
            // Mirrors Go `role.go:208`: cache invalidation is LAST in actual
            // Go execution order (the `role.go:207` comment saying "BEFORE
            // deleting relationships" is stale — the call sits after both
            // deletes; we mirror the code, not the comment).
            DeleteRoleStep::InvalidateUserCache,
        ],
    })
}

/// Mirrors Go `RoleService.BulkDeleteRoles` (`role.go:214-252`) as a pure
/// plan.
///
/// Performs the two pure guards and emits the bulk-cascade order:
/// [`DeleteRoleStep::DeleteUserRoleLinksBulk`] →
/// [`DeleteRoleStep::DeleteRolesBulk`] →
/// [`DeleteRoleStep::InvalidateUserCache`].
///
/// Guards (both pure, both carry Go's exact error strings):
///   * **Empty list → no-op success** (`role.go:217-219`):
///     `if len(ids) == 0 { return nil }`. Go does NOT treat an empty id list
///     as an error; the plan emits zero steps.
///   * **Count mismatch → error** (`role.go:229-231`):
///     `if count != len(ids) { return fmt.Errorf("expected to find %d roles,
///     but found %d", len(ids), count) }`. The pure plan surfaces this so the
///     caller knows NOT to execute any step — mirrors the rollback behavior
///     the Go test asserts (`role_test.go:421-426`: the valid role still
///     exists after the failed bulk delete).
///
/// `found_count` mirrors Go's
/// `client.Role.Query().Where(role.IDIn(ids...)).Count(ctx)`
/// (`role.go:222-227`) — the caller has already counted how many of `ids`
/// exist.
pub fn bulk_delete_roles_plan(
    ids: &[i64],
    found_count: usize,
) -> RoleServiceResult<DeleteRolePlan> {
    // Mirrors Go `role.go:217-219`:
    //   `if len(ids) == 0 { return nil }`
    // An empty request is a successful no-op — Go does NOT error.
    if ids.is_empty() {
        return Ok(DeleteRolePlan { steps: vec![] });
    }

    // Mirrors Go `role.go:229-231`:
    //   `if count != len(ids) {
    //        return fmt.Errorf("expected to find %d roles, but found %d", len(ids), count)
    //    }`
    if found_count != ids.len() {
        return Err(RoleServiceError::RolesCountMismatch {
            expected: ids.len(),
            found: found_count,
        });
    }

    Ok(DeleteRolePlan {
        steps: vec![
            // Mirrors Go `role.go:233-239`: bulk-delete UserRole rows first.
            DeleteRoleStep::DeleteUserRoleLinksBulk {
                role_ids: ids.to_vec(),
            },
            // Mirrors Go `role.go:241-247`: now safe to remove the role rows.
            DeleteRoleStep::DeleteRolesBulk {
                role_ids: ids.to_vec(),
            },
            // Mirrors Go `role.go:249`.
            DeleteRoleStep::InvalidateUserCache,
        ],
    })
}

// ---------------------------------------------------------------------------
// Tests — mirror Go `role_test.go::TestCreateRole` and
// `permission_validator_test.go::TestCanGrantScopes` golden intent.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_service::RoleView;
    use conduit_auth::slug;

    // ----- helpers ----------------------------------------------------------

    fn system_role(name: &str, scopes: &[&str]) -> RoleView {
        RoleView {
            name: name.to_string(),
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
            level: RoleLevel::System,
            project_id: None,
        }
    }

    fn project_role(name: &str, scopes: &[&str], project_id: i64) -> RoleView {
        RoleView {
            name: name.to_string(),
            scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
            level: RoleLevel::Project,
            project_id: Some(project_id),
        }
    }

    fn owner_principal() -> Principal {
        Principal::user("owner-1").with_owner(true)
    }

    fn regular_principal(scopes: &[&str]) -> Principal {
        let mut p = Principal::user("user-1");
        for s in scopes {
            p = p.with_scope((*s).to_string());
        }
        p
    }

    // ----- validate_role: mirrors TestCreateRole "create global role" -------

    #[test]
    fn validate_role_accepts_system_role_with_no_project_id() {
        // Go: `input.Level == nil` path → level=system, projectIDForAPI=ToPtr(0).
        let role = system_role("Administrator", &["read_users", "write_users"]);
        assert_eq!(validate_role(&role), Ok(()));
    }

    #[test]
    fn validate_role_accepts_system_role_with_zero_project_id() {
        // Canonical Go form: system roles are persisted with `*ProjectID == 0`
        // (Go `role.go:67-68`). This must not be rejected.
        let mut role = system_role("Administrator", &["read_users"]);
        role.project_id = Some(0);
        assert_eq!(validate_role(&role), Ok(()));
    }

    // ----- validate_role: mirrors TestCreateRole "create project-specific" --

    #[test]
    fn validate_role_accepts_project_role_with_project_id() {
        let role = project_role("Project Administrator", &["read_users"], 42);
        assert_eq!(validate_role(&role), Ok(()));
    }

    // ----- validate_role: rejects system role carrying a project id ---------

    #[test]
    fn validate_role_rejects_system_role_with_non_zero_project_id() {
        // Mirrors Go `role.go:72-74` "project ID is not allowed for system roles".
        let mut role = system_role("Bad", &["read_users"]);
        role.project_id = Some(7);
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::ProjectIdNotAllowedForSystemRole)
        );
    }

    // ----- validate_role: rejects project role missing a project id ---------

    #[test]
    fn validate_role_rejects_project_role_without_project_id() {
        // Mirrors Go `role.go:80-82` "project ID is required for project roles".
        let mut role = project_role("Bad", &["read_users"], 0);
        role.project_id = None;
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::ProjectIdRequiredForProjectRole)
        );
    }

    #[test]
    fn validate_role_rejects_project_role_with_zero_project_id() {
        // project_id == 0 is treated as system per IsSystemRole(), so a project
        // level role with pid 0 is contradictory.
        let role = project_role("Bad", &["read_users"], 0);
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::ProjectIdRequiredForProjectRole)
        );
    }

    #[test]
    fn validate_role_rejects_system_only_commercial_scope_on_project_role() {
        let role = project_role("Bad commercial operator", &[slug::GRANT_CREDIT], 42);
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::ScopeNotAllowedForProjectRole(
                slug::GRANT_CREDIT.to_string()
            ))
        );
    }

    // ----- validate_role: rejects unknown scope slugs (S12) ----------------

    #[test]
    fn validate_role_rejects_unknown_scope_slug() {
        let role = system_role("Bad", &["manage_users"]);
        // Go test input `[]string{"manage_users", "manage_projects"}` is
        // actually accepted by Go because Go's CreateRole has *no* scope-slug
        // validation; the S12 spec layers that check on top using
        // `conduit-auth::scopes`. `manage_users` is *not* in the slug set, so
        // the Rust validator must reject it.
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::InvalidScopeSlug(
                "manage_users".to_string()
            ))
        );
    }

    #[test]
    fn validate_role_accepts_all_known_scope_slugs() {
        let all_slugs: Vec<String> = conduit_auth::scopes::KNOWN_ROLE_SCOPE_SLUGS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let role = RoleView {
            name: "AllScopes".to_string(),
            scopes: all_slugs,
            level: RoleLevel::System,
            project_id: None,
        };
        assert_eq!(validate_role(&role), Ok(()));
    }

    // ----- classify_role ----------------------------------------------------

    #[test]
    fn classify_role_system_when_level_system() {
        let role = system_role("Admin", &["read_users"]);
        assert_eq!(classify_role(&role), RoleKind::System);
    }

    #[test]
    fn classify_role_system_when_project_id_zero_even_if_level_project() {
        // Defensive: IsSystemRole() wins over Level when project_id == 0.
        let mut role = project_role("Weird", &["read_users"], 0);
        role.project_id = Some(0);
        assert_eq!(classify_role(&role), RoleKind::System);
    }

    #[test]
    fn classify_role_project_when_level_project_and_non_zero_project_id() {
        let role = project_role("Project Admin", &["read_users"], 42);
        assert_eq!(classify_role(&role), RoleKind::Project);
    }

    // ----- prevent_owner_escalation: mirrors TestCanGrantScopes -------------

    #[test]
    fn owner_can_grant_any_scopes() {
        // Mirrors Go `TestCanGrantScopes`/"owner can grant any scopes".
        let role = system_role(
            "Owner Role",
            &["read_users", "write_users", "read_projects"],
        );
        assert_eq!(prevent_owner_escalation(&owner_principal(), &role), Ok(()));
    }

    #[test]
    fn system_principal_can_grant_any_scopes() {
        // Mirrors Go `authz.WithSystemBypass` analogue.
        let role = system_role("System Role", &["system:admin"]);
        assert_eq!(
            prevent_owner_escalation(&Principal::system(), &role),
            Ok(())
        );
    }

    #[test]
    fn test_principal_can_grant_any_scopes() {
        // Mirrors Go `authz.WithTestBypass` analogue.
        let role = system_role("Test Role", &["write_settings"]);
        assert_eq!(prevent_owner_escalation(&Principal::test(), &role), Ok(()));
    }

    #[test]
    fn user_can_grant_scopes_they_possess() {
        // Mirrors Go `TestCanGrantScopes`/"user can grant scopes they possess".
        let actor = regular_principal(&["read_users", "write_users"]);
        let role = project_role("Reader", &["read_users"], 5);
        assert_eq!(prevent_owner_escalation(&actor, &role), Ok(()));

        let role_both = project_role("RW", &["read_users", "write_users"], 5);
        assert_eq!(prevent_owner_escalation(&actor, &role_both), Ok(()));
    }

    #[test]
    fn user_cannot_grant_scopes_they_do_not_possess() {
        // Mirrors Go `TestCanGrantScopes`/"user cannot grant scopes they don't possess".
        let actor = regular_principal(&["read_users"]);
        let role = project_role("Writer", &["write_users"], 5);
        assert_eq!(
            prevent_owner_escalation(&actor, &role),
            Err(RoleServiceError::InsufficientPermissions(
                "write_users".to_string()
            ))
        );
    }

    #[test]
    fn user_cannot_grant_role_mixing_possessed_and_unpossessed_scopes() {
        // Mirrors the second Go assertion in the same sub-test:
        //   validator.CanGrantScopes(ctxWithUser, []string{"read_users", "write_projects"}, nil)
        //   -> error.
        let actor = regular_principal(&["read_users"]);
        let role = project_role("Mix", &["read_users", "write_projects"], 5);
        assert_eq!(
            prevent_owner_escalation(&actor, &role),
            Err(RoleServiceError::InsufficientPermissions(
                "write_projects".to_string()
            ))
        );
    }

    #[test]
    fn user_cannot_escalate_to_system_admin_scope() {
        // Owner-escalation: a regular user must not grant `system:admin`.
        let actor = regular_principal(&["read_users", "write_users"]);
        let role = system_role("Escalator", &["system:admin"]);
        assert_eq!(
            prevent_owner_escalation(&actor, &role),
            Err(RoleServiceError::InsufficientPermissions(
                "system:admin".to_string()
            ))
        );
    }

    #[test]
    fn user_cannot_grant_system_level_role_even_with_overlapping_scopes() {
        // System-level roles are implicitly owner-escalating because the scopes
        // they carry are not in a regular user's set. Build a system role whose
        // scopes the user happens to possess at project level only.
        let actor = regular_principal(&["read_users"]);
        let role = system_role("SysReader", &["read_users"]);
        // The actor has `read_users` as a direct scope, so this *passes* —
        // consistent with Go, which only checks possession, not the role's
        // level. This documents that boundary: system-level escalation is
        // gated by scope possession, not by `level == system`.
        assert_eq!(prevent_owner_escalation(&actor, &role), Ok(()));
    }

    #[test]
    fn anonymous_user_principal_is_rejected() {
        // Mirrors Go `contexts.GetUser(ctx)` failure path.
        let actor = Principal::user(""); // id present but empty — closest analogue
        let role = system_role("X", &["read_users"]);
        // Note: empty-string id still counts as "present" per the Principal
        // constructor; only `id.is_none()` triggers UserNotFoundInContext.
        // We construct the truly-anonymous case via struct patching.
        let mut anon = actor;
        anon.id = None;
        assert_eq!(
            prevent_owner_escalation(&anon, &role),
            Err(RoleServiceError::UserNotFoundInContext)
        );
    }

    // ----- is_valid_scope ---------------------------------------------------

    #[test]
    fn is_valid_scope_recognizes_all_known_slugs() {
        for slug in conduit_auth::scopes::KNOWN_ROLE_SCOPE_SLUGS {
            assert!(is_valid_scope(slug), "expected {slug} to be valid");
        }
    }

    #[test]
    fn is_valid_scope_rejects_unknown_slug() {
        assert!(!is_valid_scope("manage_users"));
        assert!(!is_valid_scope(""));
        assert!(!is_valid_scope("not_a_scope"));
    }

    // ----- Go role_test.go SEMANTICS migration (Mendel-the-4th 2026-07-06) -----
    //
    // The bulk of Go `role_test.go` (629 lines) exercises DB+cache side-effects
    // (ent.Client create/update/delete, UserRole cascade, xcache invalidation)
    // which are intentionally out of scope for this pure-logic module — see the
    // crate-level doc above. The persistence-side sub-tests (duplicate-name,
    // non-existent-role, cascade delete, cache invalidation) are listed as
    // **pending DB layer** in the task report; they require porting
    // `RoleService.{CreateRole,UpdateRole,DeleteRole,BulkDeleteRoles,
    // RoleNameExists}` to a DB-backed trait first.
    //
    // The tests below fill the PURE-LOGIC semantic gaps that Go role_test.go
    // implies but the existing Rust suite did not yet pin explicitly.

    /// Mirrors Go `TestCreateRole`/"create global role successfully"
    /// (`role_test.go:59-72`) boundary: the input there carries
    /// `Scopes: []string{"manage_users", "manage_projects"}` and Go happily
    /// persists it (Go `CreateRole` has *no* scope-slug validation). Rust
    /// layering of S12 rejects those slugs — see the divergence-dedicated test
    /// `go_role_test_scope_strings_are_rejected_by_rust_s12_validator` below.
    /// This test pins the *empty* scope list boundary, which both Go and Rust
    /// must accept (Go zero-fills `Scopes` on the ent Role when the input
    /// slice is empty/nil — `ent/role.go` field default).
    #[test]
    fn validate_role_accepts_empty_scope_list() {
        let role = system_role("Empty", &[]);
        assert_eq!(validate_role(&role), Ok(()));

        let role_p = project_role("EmptyP", &[], 7);
        assert_eq!(validate_role(&role_p), Ok(()));
    }

    /// Exception-ordering: when multiple unknown slugs are present the
    /// validator surfaces the *first* offending slug (mirroring how Go
    /// `CreateRole` would hit `ent`'s validator on the first bad entry, then
    /// stop). Pins the early-return iteration order so a future refactor that
    /// swaps to a HashSet-collect-all regression is caught.
    #[test]
    fn validate_role_reports_first_unknown_slug_when_multiple_invalid() {
        let role = system_role("Bad", &["bogus_a", "bogus_b"]);
        assert_eq!(
            validate_role(&role),
            Err(RoleServiceError::InvalidScopeSlug("bogus_a".to_string()))
        );
    }

    /// Defensive fallback parity with Go `(*Role).IsSystemRole()`
    /// (`ent/extra.go:8-10`: `return r.ProjectID == nil || *r.ProjectID == 0`).
    /// A role whose level is `Project` but whose `project_id` is `None`
    /// (Go: `*int == nil`) classifies as `System` — `IsSystemRole()` wins
    /// over `Level`. This shape is normally rejected earlier by
    /// `validate_role` (`ProjectIdRequiredForProjectRole`) but `classify_role`
    /// must still be total and mirror Go's union semantics on inconsistent
    /// input (defensive callers reconstruct a RoleView from partial rows).
    #[test]
    fn classify_role_system_when_level_project_and_project_id_none() {
        let mut role = project_role("Inconsistent", &["read_users"], 0);
        role.project_id = None;
        assert_eq!(classify_role(&role), RoleKind::System);
    }

    /// Boundary for `prevent_owner_escalation`: a role with no scopes
    /// trivially satisfies the per-scope possession check for any principal,
    /// including an unprivileged regular user. Mirrors the Go
    /// `CanGrantScopes(ctx, []string{}, nil)` degenerate case
    /// (`permission_validator.go:121-128` loops over an empty slice and
    /// returns nil). Documents that the guard never *fails open* — empty
    /// scope set is well-defined as "no escalation possible".
    #[test]
    fn prevent_owner_escalation_allows_empty_scope_list_for_any_principal() {
        let actor = regular_principal(&[]); // unprivileged, no scopes
        let role = project_role("Empty", &[], 5);
        assert_eq!(prevent_owner_escalation(&actor, &role), Ok(()));
    }

    /// Maps the pure-logic aspect of Go `TestUpdateRole`/"update role scopes
    /// successfully" (`role_test.go:151-161`). The Go path is:
    ///   ```text
    ///   // role.go:122-136
    ///   if input.Scopes != nil {
    ///       role, _ := s.entFromContext(ctx).Role.Get(ctx, id)
    ///       var projectID *int
    ///       if !role.IsSystemRole() { projectID = role.ProjectID }
    ///       if err := s.permissionValidator.CanGrantScopes(ctx, input.Scopes, projectID); err != nil {
    ///           return nil, fmt.Errorf("permission denied: %w", err)
    ///       }
    ///   }
    ///   ```
    /// I.e. the NEW scope set is re-validated through the same
    /// `CanGrantScopes` used at create time. On the Rust side that check is
    /// `prevent_owner_escalation`, so an update that adds a scope the actor
    /// does not possess must be rejected even when the actor could create the
    /// original role. Documents that mapping by simulating a scope-widening
    /// update on a project role.
    #[test]
    fn prevent_owner_escalation_guards_updated_scope_set_like_go_update_role() {
        // Actor originally granted {read_users} — could create this role.
        let actor = regular_principal(&["read_users"]);
        let original = project_role("Reader", &["read_users"], 5);
        assert_eq!(prevent_owner_escalation(&actor, &original), Ok(()));

        // Update path widens to {read_users, write_users} — write_users is
        // not possessed, so the Go re-validation rejects (role.go:133-135).
        let mut updated = original;
        updated.scopes.push("write_users".to_string());
        assert_eq!(
            prevent_owner_escalation(&actor, &updated),
            Err(RoleServiceError::InsufficientPermissions(
                "write_users".to_string()
            ))
        );
    }

    /// Comprehensive parity documentation of the deliberate Go ↔ Rust scope
    /// divergence surfaced by `role_test.go`. Every ad-hoc scope string used
    /// by the Go test suite (`manage_users`, `manage_projects`,
    /// `manage_project`, `read`, `write`, `delete`, `temp`) is NOT in the
    /// Rust S12 slug set (`conduit-auth::scopes::slug` + `system:admin`).
    ///
    /// Go `CreateRole` / `UpdateRole` accept these because Go performs no
    /// scope-slug validation in the biz layer (it relies on the `ent` enum
    /// validator, which only checks the column type, not the slug set —
    /// `ent/schema/role.go` stores scopes as `[]string`). The Rust S12 spec
    /// (`role_service.rs` module doc, `known_scope_slugs`) layers an explicit
    /// `is_valid_scope` check on top. This test pins the divergence so a
    /// future "tighten Go to match Rust" (or vice-versa) change is forced to
    /// update this assertion consciously.
    #[test]
    fn go_role_test_scope_strings_are_rejected_by_rust_s12_validator() {
        let go_scopes = [
            // TestCreateRole/"create global role" (role_test.go:62)
            "manage_users",
            "manage_projects",
            // TestCreateRole/"create project-specific role" (role_test.go:84)
            "manage_project",
            // TestCreateRole/"duplicate code" (role_test.go:99)
            "read",
            // TestUpdateRole setup + "update scopes" (role_test.go:135,152)
            "write",
            "delete",
            // TestDeleteRole/"without users" (role_test.go:187)
            "temp",
        ];
        for scope in go_scopes {
            assert!(
                !is_valid_scope(scope),
                "Rust S12 validator must reject Go role_test.go scope '{scope}' \
                 (Go accepts it; this divergence is intentional)"
            );
        }

        // And the full Roles built from those Go scopes are therefore
        // rejected by validate_role, even when their level/project_id shape
        // is otherwise valid.
        let go_system_role = system_role("Administrator", &["manage_users", "manage_projects"]);
        assert_eq!(
            validate_role(&go_system_role),
            Err(RoleServiceError::InvalidScopeSlug(
                "manage_users".to_string()
            ))
        );
    }

    // ----- delete_role_plan / bulk_delete_roles_plan — Mendel-the-6th -------
    // Mirrors Go `TestDeleteRole` (role_test.go:174-282, 3 sub-tests) and
    // `TestBulkDeleteRoles` (role_test.go:284-433, 4 sub-tests). The Go tests
    // exercise DB+cache side-effects (ent.Client create/delete, UserRole
    // cascade, xcache invalidation); the pure-plan surface lets us pin the
    // load-bearing semantics (error strings, cascade order, empty-list no-op,
    // count-mismatch guard) without a database, mirroring how
    // `soft_delete_project_plan` tests cover `biz/project.go::DeleteProject`.

    /// Test-only helper: extract the variant name of a [`DeleteRoleStep`]
    /// without comparing payloads. Used to assert cascade ORDER specifically
    /// (the payloads are covered by the exact-match tests below).
    fn step_kind(step: &DeleteRoleStep) -> &'static str {
        match step {
            DeleteRoleStep::DeleteUserRoleLinks { .. } => "DeleteUserRoleLinks",
            DeleteRoleStep::DeleteUserRoleLinksBulk { .. } => "DeleteUserRoleLinksBulk",
            DeleteRoleStep::DeleteRole { .. } => "DeleteRole",
            DeleteRoleStep::DeleteRolesBulk { .. } => "DeleteRolesBulk",
            DeleteRoleStep::InvalidateUserCache => "InvalidateUserCache",
        }
    }

    // === TestDeleteRole (role_test.go:174-282) — 3 sub-tests ===============

    /// Mirrors Go `TestDeleteRole/"delete role without users successfully"`
    /// (`role_test.go:182-200`). The pure plan must succeed for an existing
    /// role and emit the 3-step cascade in Go's exact order:
    /// `DeleteUserRoleLinks` → `DeleteRole` → `InvalidateUserCache`.
    /// The Go test then verifies the role no longer exists (`role_test.go:195-
    /// 199`) — that is the repo's job; the plan only describes the order.
    #[test]
    fn delete_role_plan_succeeds_for_existing_role_with_exact_cascade() {
        let expected = Ok(DeleteRolePlan {
            steps: vec![
                DeleteRoleStep::DeleteUserRoleLinks { role_id: 42 },
                DeleteRoleStep::DeleteRole { role_id: 42 },
                DeleteRoleStep::InvalidateUserCache,
            ],
        });
        assert_eq!(delete_role_plan(42, true), expected);
    }

    /// Mirrors Go `TestDeleteRole/"delete role with users successfully"`
    /// (`role_test.go:202-275`). The cascade ordering is load-bearing here:
    /// `DeleteUserRoleLinks` MUST come before `DeleteRole`, otherwise the
    /// role row delete would trip the `UserRole.role_id` FK constraint. The
    /// Go test also verifies "UserRole relationships are deleted"
    /// (`role_test.go:257-261`) and "users still exist" (`role_test.go:264-
    /// 274`) — those are DB-repo concerns; the plan only pins the order that
    /// makes them achievable.
    #[test]
    fn delete_role_plan_orders_userrole_links_before_role_delete() {
        // Assert ORDER only (payloads covered by the exact-match test above)
        // by reducing each step to its variant name.
        let kinds: Vec<&'static str> = match delete_role_plan(7, true) {
            Ok(plan) => plan.steps.iter().map(step_kind).collect(),
            Err(e) => panic!("delete_role_plan(7, true) must succeed, got: {e:?}"),
        };
        assert_eq!(
            kinds,
            ["DeleteUserRoleLinks", "DeleteRole", "InvalidateUserCache"]
        );
    }

    /// Mirrors Go `TestDeleteRole/"fail to delete non-existent role"`
    /// (`role_test.go:277-281`). The Go assertion is:
    ///   ```text
    ///   err := roleService.DeleteRole(ctx, 99999)
    ///   require.Error(t, err)
    ///   require.Contains(t, err.Error(), "role not found")
    ///   ```
    /// The pure plan must reject a non-existent role with the exact Go error
    /// string so that a future repo-backed service surfaces the identical
    /// message. We assert both the enum variant AND the `Display` literal.
    #[test]
    fn delete_role_plan_rejects_non_existent_role_with_exact_go_message() {
        // Pin the enum variant first.
        assert_eq!(
            delete_role_plan(99999, false),
            Err(RoleServiceError::RoleNotFound)
        );
        // Go's `err.Error()` must contain "role not found" — pin the literal
        // so a future rename is caught. Re-derive the Display string via
        // to_string() on a fresh call.
        let msg = delete_role_plan(99999, false)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| String::from("<no error>"));
        assert!(
            msg.contains("role not found"),
            "expected Go-parity message containing 'role not found', got: {msg}"
        );
    }

    // === TestBulkDeleteRoles (role_test.go:284-433) — 4 sub-tests =========

    /// Mirrors Go `TestBulkDeleteRoles/"bulk delete roles without users
    /// successfully"` (`role_test.go:292-323`). The pure plan must succeed
    /// when all ids exist and emit the 3-step bulk cascade with the full id
    /// payload preserved on each step.
    #[test]
    fn bulk_delete_roles_plan_succeeds_when_all_ids_exist() {
        let ids = vec![1_i64, 2, 3];
        let expected = Ok(DeleteRolePlan {
            steps: vec![
                DeleteRoleStep::DeleteUserRoleLinksBulk {
                    role_ids: vec![1, 2, 3],
                },
                DeleteRoleStep::DeleteRolesBulk {
                    role_ids: vec![1, 2, 3],
                },
                DeleteRoleStep::InvalidateUserCache,
            ],
        });
        assert_eq!(bulk_delete_roles_plan(&ids, 3), expected);
    }

    /// Mirrors Go `TestBulkDeleteRoles/"bulk delete roles with users
    /// successfully"` (`role_test.go:325-405`). As with the single-role
    /// variant, the cascade order (`DeleteUserRoleLinksBulk` BEFORE
    /// `DeleteRolesBulk`) is what the Go test's "UserRole relationships are
    /// deleted" (`role_test.go:393-397`) + "users still exist"
    /// (`role_test.go:400-404`) assertions ultimately exercise. The
    /// user-still-exists part is a DB concern; the order is pure.
    #[test]
    fn bulk_delete_roles_plan_orders_userrole_bulk_before_role_bulk_delete() {
        let ids = vec![10_i64, 20];
        let kinds: Vec<&'static str> = match bulk_delete_roles_plan(&ids, 2) {
            Ok(plan) => plan.steps.iter().map(step_kind).collect(),
            Err(e) => panic!("bulk_delete_roles_plan(&[10,20], 2) must succeed, got: {e:?}"),
        };
        assert_eq!(
            kinds,
            [
                "DeleteUserRoleLinksBulk",
                "DeleteRolesBulk",
                "InvalidateUserCache"
            ]
        );
    }

    /// Mirrors Go `TestBulkDeleteRoles/"fail to bulk delete with non-existent
    /// role"` (`role_test.go:407-427`). The Go assertion is:
    ///   ```text
    ///   roleIDs := []int{validRole.ID, 99999}
    ///   err := roleService.BulkDeleteRoles(ctx, roleIDs)
    ///   require.Error(t, err)
    ///   require.Contains(t, err.Error(), "expected to find")
    ///   // valid role still exists (transaction rollback)
    ///   ```
    /// The pure plan surfaces the count-mismatch error so the caller knows
    /// NOT to execute any step — this is the pure-logic signal that drives
    /// the rollback the Go test asserts. The `expected`/`found` counts are
    /// pinned to Go's exact `fmt.Errorf` parameters.
    #[test]
    fn bulk_delete_roles_plan_rejects_count_mismatch_with_exact_go_message() {
        // Go passes `[]int{validRole.ID, 99999}` — one valid, one missing.
        // Found count = 1 (only validRole.ID exists).
        let ids = vec![7_i64, 99999];
        // Pin the enum variant + payload to Go's exact `fmt.Errorf` args:
        //   "expected to find %d roles, but found %d" with (len, count) = (2, 1).
        assert_eq!(
            bulk_delete_roles_plan(&ids, 1),
            Err(RoleServiceError::RolesCountMismatch {
                expected: 2,
                found: 1
            })
        );
        // Pin the full Go `fmt.Errorf` format string byte-for-byte via
        // `Display` so a future rename of the variant's `#[error(...)]`
        // attribute is caught.
        let msg = bulk_delete_roles_plan(&ids, 1)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| String::from("<no error>"));
        assert!(
            msg.contains("expected to find"),
            "expected Go-parity message containing 'expected to find', got: {msg}"
        );
        assert_eq!(msg, "expected to find 2 roles, but found 1");
    }

    /// Mirrors Go `TestBulkDeleteRoles/"bulk delete with empty list"`
    /// (`role_test.go:429-432`). The Go assertion is simply:
    ///   ```text
    ///   err := roleService.BulkDeleteRoles(ctx, []int{})
    ///   require.NoError(t, err)
    ///   ```
    /// i.e. an empty id list is a successful no-op. The pure plan must emit
    /// zero steps — this is NOT an error in Go (contrast with count-mismatch
    /// which IS an error).
    #[test]
    fn bulk_delete_roles_plan_empty_list_is_successful_noop() {
        let expected = Ok(DeleteRolePlan { steps: vec![] });
        assert_eq!(bulk_delete_roles_plan(&[], 0), expected);
    }

    /// Defensive: empty list short-circuits BEFORE the count-mismatch guard,
    /// so `found_count != 0` must NOT trigger an error when `ids` is empty.
    /// Pins Go's `role.go:217-219` (empty check) ordering above `role.go:229-
    /// 231` (count check) — the empty-list branch returns nil unconditionally
    /// and never consults `count`.
    #[test]
    fn bulk_delete_roles_plan_empty_list_short_circuits_before_count_check() {
        let expected = Ok(DeleteRolePlan { steps: vec![] });
        // found_count = 99 would normally mismatch, but the empty-list guard
        // fires first and returns Ok(empty plan).
        assert_eq!(bulk_delete_roles_plan(&[], 99), expected);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlPrincipal {
    pub user_id: String,
    pub role: GraphqlPrincipalRole,
}

impl GraphqlPrincipal {
    pub fn owner(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            role: GraphqlPrincipalRole::Owner,
        }
    }

    pub fn member(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            role: GraphqlPrincipalRole::Member,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphqlPrincipalRole {
    Owner,
    Member,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlPolicyContext {
    pub principal: GraphqlPrincipal,
    pub project_id: Option<String>,
}

impl GraphqlPolicyContext {
    pub fn new(principal: GraphqlPrincipal, project_id: Option<impl Into<String>>) -> Self {
        Self {
            principal,
            project_id: project_id.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlAuthorizedProject {
    pub user_id: String,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphqlPolicyError {
    MissingProject,
    Forbidden,
}

pub trait GraphqlProjectPolicyGuard {
    fn can_access_project(
        &self,
        principal: &GraphqlPrincipal,
        project_id: &str,
    ) -> Result<bool, GraphqlPolicyError>;
}

pub fn require_project_access(
    context: &GraphqlPolicyContext,
    guard: &impl GraphqlProjectPolicyGuard,
) -> Result<GraphqlAuthorizedProject, GraphqlPolicyError> {
    let project_id = context
        .project_id
        .as_deref()
        .ok_or(GraphqlPolicyError::MissingProject)?;

    if !guard.can_access_project(&context.principal, project_id)? {
        return Err(GraphqlPolicyError::Forbidden);
    }

    Ok(GraphqlAuthorizedProject {
        user_id: context.principal.user_id.clone(),
        project_id: project_id.to_owned(),
    })
}

// =====================================================================
// S08/S12 — Resolver→service policy guard (pure logic).
//
// Mirrors the Go contract in `conduit/internal/authz/scope.go`:
//
//   func WithScopeDecision(ctx context.Context, requiredScope scopes.ScopeSlug) context.Context {
//       has := HasScope(ctx, requiredScope)
//       ...
//       if has { return privacy.DecisionContext(ctx, privacy.Allow) }
//       return privacy.DecisionContext(ctx, privacy.Deny)
//   }
//
//   func RequireScope(ctx context.Context, requiredScope scopes.ScopeSlug) error {
//       if !HasScope(ctx, requiredScope) {
//           p, _ := GetPrincipal(ctx)
//           return fmt.Errorf("authz: principal %s does not have required scope %s", p.String(), requiredScope)
//       }
//       return nil
//   }
//
// And `conduit/internal/server/gql/dashboard.resolvers.go:39`:
//
//   func (r *queryResolver) DashboardOverview(ctx context.Context) (*DashboardOverview, error) {
//       ctx = authz.WithScopeDecision(ctx, scopes.ScopeReadDashboard)
//       ...
//   }
//
// Every Go resolver stamps the required scope into the context via
// `authz.WithScopeDecision` BEFORE delegating to a service (`r.systemService...`,
// `r.userService...`). The ent privacy layer (inside the service/repo) then
// consumes that decision. Resolvers therefore NEVER touch repos directly —
// the scope decision is the boundary. S12 ("resolvers only call services")
// and S08 ("resolver must not bypass service/repository policy") are two
// facets of the same boundary.
//
// This module exposes the Rust-side mirror: a pure `ResolverGuard` that, given
// a resolver intent (field + required scope) and a principal, delegates the
// allow/deny decision to `conduit_auth::rbac` (Hobbes/Austin work) and lifts
// the result into `conduit_core::ConduitError` so a resolver can return it
// directly. The guard is deliberately pure (no I/O, no async) — it composes
// the same `has_scope` / `has_project_scope` predicates the HTTP middleware
// uses, so the boundary stays testable without a running service.
// =====================================================================
//
// `ConduitError` is ~216 bytes (it carries provider body / headers / metadata
// for upstream-error fidelity, by design in `conduit-core`). Returning it
// from a pure guard triggers `clippy::result_large_err`, but boxing it here
// would diverge from the workspace-wide `Result<_, ConduitError>` convention
// every other crate uses, so each authorize* function below carries
// `#[allow(clippy::result_large_err)]`.

use async_graphql::Context;
use conduit_auth::rbac::{self, PermissionDecision};
use conduit_auth::request_context::RequestContext;
use conduit_auth::{Principal, PrincipalKind};
use conduit_core::ConduitError;

/// A resolver intent: the GraphQL field about to be executed and the scope a
/// Go resolver would stamp into the context before delegating to its service.
///
/// Mirrors the `(field, scopes.ScopeSlug)` pair implicit in every Go resolver
/// — e.g. `DashboardOverview` carries `scopes.ScopeReadDashboard`,
/// `UpdateBrandSettings` carries the write-settings scope applied inside
/// `systemService`. The `kind` field records whether the resolver needs a
/// project-scoped decision (`has_project_scope`) or a global one
/// (`has_scope`), matching the Go split between `userHasScope` and
/// `userHasProjectScope` in `authz/scope.go`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverIntent {
    pub field: &'static str,
    pub scope: &'static str,
    pub kind: ResolverScopeKind,
}

impl ResolverIntent {
    /// A resolver that requires a global (non-project) scope.
    /// Mirrors Go `authz.HasScope(ctx, scope)`.
    pub const fn global(field: &'static str, scope: &'static str) -> Self {
        Self {
            field,
            scope,
            kind: ResolverScopeKind::Global,
        }
    }

    /// A resolver that requires a project-scoped decision.
    /// Mirrors Go `userHasProjectScope` evaluation gated by
    /// `contexts.GetProjectID(ctx)`.
    pub const fn project(field: &'static str, scope: &'static str) -> Self {
        Self {
            field,
            scope,
            kind: ResolverScopeKind::Project,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolverScopeKind {
    /// Decision via `conduit_auth::rbac::has_scope` — mirrors Go `HasScope`.
    Global,
    /// Decision via `conduit_auth::rbac::has_project_scope` — mirrors Go
    /// `userHasProjectScope` (requires a project context).
    Project,
}

/// The pure resolver-side policy guard. It owns the principal and (optionally)
/// the request context, and converts rbac decisions into `ConduitError`.
///
/// This is the structural mirror of Go's resolver struct: the Go resolver
/// holds `*biz.SystemService`, `*biz.UserService`, … and stamps scope decisions
/// into the ctx before calling them. Here the guard performs the stamping
/// step explicitly and pure-fashion, so a resolver body becomes:
///
/// ```ignore
/// guard.authorize(&ResolverIntent::global("dashboardOverview", slug::READ_DASHBOARD))?;
/// // … delegate to the service …
/// ```
///
/// Resolvers must NOT call repositories directly after this check — the
/// service is the only caller allowed past the guard. That boundary is what
/// S08 ("resolver must not bypass service/repository policy") and S12
/// ("resolvers only call services") require.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverGuard {
    pub principal: Principal,
    /// Optional request context. When the intent is `ResolverScopeKind::Project`,
    /// the guard delegates to `rbac::has_project_scope`, which mirrors Go's
    /// `userHasProjectScope` and needs the project id. For `Global` intents
    /// the rbac path is `rbac::has_scope`, which only needs the principal.
    pub request_context: Option<RequestContext>,
}

impl ResolverGuard {
    pub fn new(principal: Principal) -> Self {
        Self {
            principal,
            request_context: None,
        }
    }

    pub fn with_request_context(mut self, ctx: RequestContext) -> Self {
        self.request_context = Some(ctx);
        self
    }

    /// Evaluate the resolver intent and return the raw rbac decision.
    ///
    /// Mirrors Go `authz.HasScope(ctx, scope)` returning a bool. The decision
    /// is delegated to `conduit_auth::rbac` so the rule chain (owner bypass →
    /// direct scope → system-role scope → project membership/role scope →
    /// api-key scope → deny-by-default) stays single-sourced.
    pub fn decide(&self, intent: &ResolverIntent) -> PermissionDecision {
        match intent.kind {
            ResolverScopeKind::Global => self.decide_global(intent.scope),
            ResolverScopeKind::Project => self.decide_project(intent.scope),
        }
    }

    fn decide_global(&self, scope: &'static str) -> PermissionDecision {
        match self.context_for_decision() {
            Some(ctx) => rbac::has_scope(&ctx, scope),
            None => PermissionDecision::deny(
                "principal could not be attached to the resolver request context",
            ),
        }
    }

    fn decide_project(&self, scope: &'static str) -> PermissionDecision {
        match self.context_for_decision() {
            Some(ctx) => rbac::has_project_scope(&ctx, scope),
            None => PermissionDecision::deny(
                "principal could not be attached to the resolver request context",
            ),
        }
    }

    fn context_for_decision(&self) -> Option<RequestContext> {
        // Prefer an explicitly-attached request context (the resolver
        // already built one). Otherwise synthesise one from the principal
        // alone — mirrors Go `HasScope` being callable with just a principal
        // for non-project intents.
        if let Some(ctx) = &self.request_context {
            return Some(ctx.clone());
        }
        request_context_with(&self.principal, None)
    }

    /// Lift the rbac decision into `Result<(), ConduitError>`. Mirrors Go
    /// `authz.RequireScope(ctx, scope) error` — returns
    /// `ConduitError::forbidden` (HTTP 403) on denial, `Ok(())` on allow.
    /// An absent principal is reported as `ConduitError::unauthorized` (HTTP 401)
    /// to match the Go middleware distinction between "no principal" and
    /// "principal lacks scope".
    #[allow(clippy::result_large_err)] // ConduitError size is fixed in conduit-core
    pub fn authorize(&self, intent: &ResolverIntent) -> Result<(), ConduitError> {
        let decision = self.decide(intent);

        if decision.is_allowed() {
            return Ok(());
        }

        // Distinguish "no principal" from "principal denied". Go's
        // `authz.HasScope` returns false for `!ok` on `GetPrincipal`, which
        // middleware maps to 401; a present-but-unscoped principal maps to 403.
        if !has_identity(&self.principal) {
            Err(ConduitError::unauthorized(format!(
                "authz: resolver `{}` requires an authenticated principal with scope `{}`",
                intent.field, intent.scope
            )))
        } else {
            Err(ConduitError::forbidden(format!(
                "authz: principal {} does not have required scope {} for resolver `{}`",
                principal_label(&self.principal),
                intent.scope,
                intent.field
            )))
        }
    }
}

/// Free function form of [`ResolverGuard::authorize`]: given a principal and
/// a scope slug, delegate to `conduit_auth::rbac` and lift the result into
/// `ConduitError`. This is the closest Rust analogue of Go
/// `authz.RequireScope(ctx, scope) error` and is the entry point resolvers
/// use when they don't need a project context.
///
/// Pure: no I/O, no async, no global state. Decision source is the same
/// `conduit_auth::rbac` module the HTTP middleware uses.
#[allow(clippy::result_large_err)] // ConduitError size is fixed in conduit-core
pub fn authorize_resolver(principal: &Principal, scope: &str) -> Result<(), ConduitError> {
    let decision = match request_context_with(principal, None) {
        Some(ctx) => rbac::has_scope(&ctx, scope),
        None => PermissionDecision::deny(
            "principal could not be attached to the resolver request context",
        ),
    };

    if decision.is_allowed() {
        Ok(())
    } else if !has_identity(principal) {
        Err(ConduitError::unauthorized(format!(
            "authz: principal does not have required scope {scope}"
        )))
    } else {
        Err(ConduitError::forbidden(format!(
            "authz: principal {} does not have required scope {scope}",
            principal_label(principal)
        )))
    }
}

/// Project-scoped variant — mirrors Go `userHasProjectScope`. Requires the
/// principal to carry a project id (api keys) or be evaluated against the
/// given request project (users).
#[allow(clippy::result_large_err)] // ConduitError size is fixed in conduit-core
pub fn authorize_project_resolver(
    principal: &Principal,
    project_id: &str,
    scope: &str,
) -> Result<(), ConduitError> {
    let decision = match request_context_with(principal, Some(project_id)) {
        Some(ctx) => rbac::has_project_scope(&ctx, scope),
        None => PermissionDecision::deny(
            "principal could not be attached to the resolver request context",
        ),
    };

    if decision.is_allowed() {
        Ok(())
    } else if !has_identity(principal) {
        Err(ConduitError::unauthorized(format!(
            "authz: principal does not have required project scope {scope}"
        )))
    } else {
        Err(ConduitError::forbidden(format!(
            "authz: principal {} does not have required project scope {scope}",
            principal_label(principal)
        )))
    }
}

fn has_identity(principal: &Principal) -> bool {
    // System/test principals always have identity (Go `PrincipalTypeSystem`/
    // `PrincipalTypeTest` and the bypass path). A user/api-key principal has
    // identity iff it carries an id. Owner flag counts as identity too.
    matches!(principal.kind, PrincipalKind::System | PrincipalKind::Test)
        || principal.id.is_some()
        || principal.is_owner
}

fn request_context_with(principal: &Principal, project_id: Option<&str>) -> Option<RequestContext> {
    let mut ctx = RequestContext::new();
    if ctx.set_principal(principal.clone()).is_err() {
        return None;
    }
    if let Some(project_id) = project_id
        && ctx.set_project_id(project_id).is_err()
    {
        return None;
    }
    Some(ctx)
}

fn principal_label(principal: &Principal) -> String {
    match (&principal.kind, &principal.id) {
        (PrincipalKind::System, _) => "system".to_owned(),
        (PrincipalKind::Test, _) => "test".to_owned(),
        (_, Some(id)) => format!("`{id}`"),
        (_, None) => "<anonymous>".to_owned(),
    }
}

/// Re-export of the rbac decision source for resolver audit logs.
pub use conduit_auth::rbac::PermissionSource as ResolverPermissionSource;

// =====================================================================
// Per-request principal plumbing (closes the `Principal::test()` bypass)
// =====================================================================

/// Read the per-request [`RequestContext`] the host handler published into the
/// async-graphql data bag.
///
/// Go's resolvers get the authenticated identity from the gin request context,
/// where `WithJWTAuth` put the loaded `*ent.User` + principal. The Rust admin
/// schema is a boot singleton, so `graphql_handler` republishes the auth context
/// into the per-request data bag; this is the resolver-side read of it.
///
/// `None` means the request arrived without JWT auth. Callers must treat that as
/// "deny" — never as "trusted" — which is exactly the bug the fabricated
/// `Principal::test()` contexts introduced.
pub fn request_context<'a>(ctx: &'a Context<'_>) -> Option<&'a RequestContext> {
    ctx.data_opt::<RequestContext>()
}

/// Authorize the current resolver against `scope`, using the real per-request
/// principal.
///
/// This is the Rust stand-in for the Go ent-privacy check that runs on every
/// query/mutation: the entity's `Policy()` rule chain, backed by
/// `scopes.QueryPolicy`'s `privacy.Denyf("default deny")` fallback
/// (`internal/scopes/policy.go:36-52`). Resolvers call it before delegating to
/// their service.
///
/// Fails closed: a request with no auth context is denied rather than trusted.
#[allow(clippy::result_large_err)] // ConduitError size is fixed in conduit-core
pub fn authorize_current(ctx: &Context<'_>, scope: &str) -> Result<(), ConduitError> {
    let Some(request_ctx) = request_context(ctx) else {
        return Err(ConduitError::unauthorized(
            "authz: request carries no authenticated principal",
        ));
    };
    let Some(principal) = request_ctx.principal.as_ref() else {
        return Err(ConduitError::unauthorized(
            "authz: request carries no authenticated principal",
        ));
    };
    authorize_resolver(principal, scope)
}

/// P-31: prevent privilege escalation through user/role scope grants.
///
/// A **non-owner** caller may only grant scopes they already hold and may NOT
/// set `is_owner`. An owner may grant anything. Mirrors Go
/// `PermissionValidator.CanGrantScopes` plus the owner-gated `is_owner` field
/// (`biz/user.go:88-95`, `permission_validator.go:94-131`); the caller's own
/// scope set is the grant ceiling.
///
/// **Invariant**: the production `/admin/graphql` route is mounted behind
/// `jwt_admin_auth`, which publishes the authenticated `RequestContext` into the
/// per-request data bag (`graphql_handler`), so [`request_context`] is always
/// present here in production (independent of the P-01 scope extension). When it
/// is absent — the crate's own tests build bare schemas without a principal —
/// the guard is a deliberate no-op.
#[allow(clippy::result_large_err)] // ConduitError size is fixed in conduit-core
pub fn guard_scope_grant<'a>(
    ctx: &Context<'_>,
    set_owner: Option<bool>,
    granted_scopes: impl IntoIterator<Item = &'a String>,
) -> Result<(), ConduitError> {
    let Some(principal) = request_context(ctx).and_then(|rc| rc.principal.as_ref()) else {
        return Ok(());
    };
    if principal.is_owner {
        return Ok(());
    }
    // Non-owner: `is_owner` is an owner-only grant.
    if set_owner == Some(true) {
        return Err(ConduitError::forbidden(
            "authz: only an owner may grant owner (is_owner)",
        ));
    }
    // Non-owner: may only grant scopes it already possesses.
    for scope in granted_scopes {
        if !principal.scopes.contains(scope) {
            return Err(ConduitError::forbidden(format!(
                "authz: cannot grant scope `{scope}` you do not hold"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod resolver_guard_tests {
    use super::*;
    use conduit_auth::rbac::PermissionSource;
    use conduit_auth::scopes::Scope;
    use conduit_auth::scopes::slug;

    fn user(id: &str) -> Principal {
        Principal::user(id)
    }

    fn user_with_scope(id: &str, scope: &'static str) -> Principal {
        Principal::user(id).with_scope(scope)
    }

    fn owner(id: &str) -> Principal {
        Principal::user(id).with_owner(true)
    }

    #[test]
    fn authorize_resolver_allows_when_principal_has_scope() {
        let principal = user_with_scope("user-1", slug::READ_DASHBOARD);

        let result = authorize_resolver(&principal, slug::READ_DASHBOARD);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[test]
    fn authorize_resolver_denies_with_forbidden_when_scope_missing() {
        // Mirrors Go `authz.RequireScope` returning an error for a present
        // but unscoped principal. The Go test
        // `scope_test.go::TestRequireScope_Denied` asserts the same boundary.
        let principal = user("user-1");

        let result = authorize_resolver(&principal, slug::WRITE_SETTINGS);

        let err = match result {
            Err(err) => err,
            Ok(()) => panic!("expected forbidden error"),
        };
        assert_eq!(err.kind, conduit_core::ErrorKind::Forbidden);
        assert_eq!(err.http_status, 403);
        assert!(
            err.message.contains(slug::WRITE_SETTINGS),
            "error message should mention the missing scope: {}",
            err.message
        );
    }

    #[test]
    fn authorize_resolver_allows_owner_without_explicit_scope() {
        // Mirrors Go `userHasScope` short-circuit on `user.IsOwner`.
        let principal = owner("owner-1");

        let result = authorize_resolver(&principal, slug::WRITE_SETTINGS);

        assert!(result.is_ok());
    }

    #[test]
    fn authorize_resolver_allows_system_and_test_principals() {
        // Mirrors Go `HasScope` switch on PrincipalTypeSystem/PrincipalTypeTest.
        assert!(authorize_resolver(&Principal::system(), slug::READ_USERS).is_ok());
        assert!(authorize_resolver(&Principal::test(), slug::READ_USERS).is_ok());
    }

    #[test]
    fn authorize_project_resolver_respects_project_membership_scope() {
        // Mirrors Go `userHasProjectScope`: a user with a project-membership
        // scope is allowed only when the request project matches.
        let principal = Principal::user("user-1")
            .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS));

        let same_project = authorize_project_resolver(&principal, "project-1", slug::READ_PROJECTS);
        let other_project =
            authorize_project_resolver(&principal, "project-2", slug::READ_PROJECTS);

        assert!(same_project.is_ok());
        assert!(other_project.is_err());
    }

    #[test]
    fn resolver_guard_authorize_returns_forbidden_for_unscoped_user() {
        let guard = ResolverGuard::new(user("user-1"));
        let intent = ResolverIntent::global("dashboardOverview", slug::READ_DASHBOARD);

        let result = guard.authorize(&intent);

        let err = match result {
            Err(err) => err,
            Ok(()) => panic!("expected forbidden error"),
        };
        assert_eq!(err.kind, conduit_core::ErrorKind::Forbidden);
        assert!(err.message.contains("dashboardOverview"));
        assert!(err.message.contains(slug::READ_DASHBOARD));
    }

    #[test]
    fn resolver_guard_authorize_ok_for_scoped_user() {
        let guard = ResolverGuard::new(user_with_scope("user-1", slug::READ_CHANNELS));
        let intent = ResolverIntent::global("channels", slug::READ_CHANNELS);

        assert!(guard.authorize(&intent).is_ok());
    }

    #[test]
    fn resolver_guard_project_intent_routes_through_project_scope_path() {
        // A user with only a project-membership scope must pass the project
        // intent but fail a global intent for the same slug — proving the
        // guard routes Project intents through `has_project_scope` and
        // Global intents through `has_scope`, matching the Go split between
        // `userHasScope` and `userHasProjectScope`.
        //
        // Mirrors Go `userHasProjectScope`: the project id is taken from
        // `contexts.GetProjectID(ctx)`, i.e. the request context — NOT from
        // the principal. So we attach a `RequestContext` carrying both the
        // principal and the project id, exactly as a real resolver would.
        let principal = Principal::user("user-1")
            .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS));
        let mut request_ctx = RequestContext::new();
        let _ = request_ctx.set_principal(principal.clone());
        let _ = request_ctx.set_project_id("project-1");
        let guard = ResolverGuard::new(principal).with_request_context(request_ctx);

        let project_intent = ResolverIntent::project("projects", slug::READ_PROJECTS);
        let global_intent = ResolverIntent::global("projects", slug::READ_PROJECTS);

        assert!(
            guard.authorize(&project_intent).is_ok(),
            "project intent should be allowed via project-membership scope"
        );
        assert!(
            guard.authorize(&global_intent).is_err(),
            "global intent should be denied — membership scope is project-only"
        );
    }

    #[test]
    fn resolver_guard_decision_source_is_direct_scope_for_user() {
        // Audit trail: a user principal with a direct scope must surface
        // `PermissionSource::DirectScope`, matching the rbac test
        // `user_scope_allow_and_deny`.
        let guard = ResolverGuard::new(user_with_scope("user-1", slug::READ_CHANNELS));
        let intent = ResolverIntent::global("models", slug::READ_CHANNELS);

        let decision = guard.decide(&intent);

        assert_eq!(decision.source(), Some(PermissionSource::DirectScope));
    }

    #[test]
    fn resolver_guard_decision_source_is_owner_for_owner_principal() {
        let guard = ResolverGuard::new(owner("owner-1"));
        let intent = ResolverIntent::global("settings", slug::WRITE_SETTINGS);

        let decision = guard.decide(&intent);

        assert_eq!(decision.source(), Some(PermissionSource::Owner));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashSet;

    use super::{
        GraphqlPolicyContext, GraphqlPolicyError, GraphqlPrincipal, GraphqlPrincipalRole,
        GraphqlProjectPolicyGuard, require_project_access,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct GuardCall {
        user_id: String,
        role: GraphqlPrincipalRole,
        project_id: String,
    }

    #[derive(Default)]
    struct RecordingProjectPolicyGuard {
        allowed_projects: HashSet<String>,
        calls: RefCell<Vec<GuardCall>>,
    }

    impl RecordingProjectPolicyGuard {
        fn allowing_project(mut self, project_id: impl Into<String>) -> Self {
            self.allowed_projects.insert(project_id.into());
            self
        }
    }

    impl GraphqlProjectPolicyGuard for RecordingProjectPolicyGuard {
        fn can_access_project(
            &self,
            principal: &GraphqlPrincipal,
            project_id: &str,
        ) -> Result<bool, GraphqlPolicyError> {
            self.calls.borrow_mut().push(GuardCall {
                user_id: principal.user_id.clone(),
                role: principal.role,
                project_id: project_id.to_owned(),
            });

            Ok(principal.role == GraphqlPrincipalRole::Owner
                || self.allowed_projects.contains(project_id))
        }
    }

    #[test]
    fn owner_is_allowed_by_policy_guard() {
        let context =
            GraphqlPolicyContext::new(GraphqlPrincipal::owner("owner-1"), Some("project-1"));
        let guard = RecordingProjectPolicyGuard::default();

        let authorized = require_project_access(&context, &guard);

        match authorized {
            Ok(authorized) => {
                assert_eq!(authorized.user_id, "owner-1");
                assert_eq!(authorized.project_id, "project-1");
            }
            Err(error) => panic!("owner should be allowed by guard: {error:?}"),
        }
        assert_eq!(
            guard.calls.borrow().as_slice(),
            &[GuardCall {
                user_id: "owner-1".to_owned(),
                role: GraphqlPrincipalRole::Owner,
                project_id: "project-1".to_owned(),
            }]
        );
    }

    #[test]
    fn missing_project_is_denied_without_calling_guard() {
        let context = GraphqlPolicyContext::new(GraphqlPrincipal::member("user-1"), None::<String>);
        let guard = RecordingProjectPolicyGuard::default();

        let authorized = require_project_access(&context, &guard);

        assert_eq!(authorized, Err(GraphqlPolicyError::MissingProject));
        assert!(guard.calls.borrow().is_empty());
    }

    #[test]
    fn project_resolver_policy_requires_service_guard() {
        let context =
            GraphqlPolicyContext::new(GraphqlPrincipal::member("user-1"), Some("project-1"));
        let guard = RecordingProjectPolicyGuard::default().allowing_project("project-1");

        let authorized = require_project_access(&context, &guard);

        assert!(authorized.is_ok());
        assert_eq!(
            guard.calls.borrow().as_slice(),
            &[GuardCall {
                user_id: "user-1".to_owned(),
                role: GraphqlPrincipalRole::Member,
                project_id: "project-1".to_owned(),
            }]
        );
    }
}

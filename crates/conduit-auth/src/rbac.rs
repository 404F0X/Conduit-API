use serde::{Deserialize, Serialize};

use crate::principal::{Principal, PrincipalKind};
use crate::request_context::RequestContext;
use crate::scopes::ScopeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionSource {
    System,
    Test,
    Owner,
    DirectScope,
    SystemRoleScope,
    ProjectMembershipScope,
    ProjectRoleScope,
    ApiKeyScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow {
        source: PermissionSource,
        reason: String,
    },
    Deny {
        reason: String,
    },
}

impl PermissionDecision {
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    pub const fn source(&self) -> Option<PermissionSource> {
        match self {
            Self::Allow { source, .. } => Some(*source),
            Self::Deny { .. } => None,
        }
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Allow { reason, .. } | Self::Deny { reason } => reason,
        }
    }

    pub fn allow(source: PermissionSource, reason: impl Into<String>) -> Self {
        Self::Allow {
            source,
            reason: reason.into(),
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }
}

pub fn has_scope(ctx: &RequestContext, scope: impl AsRef<str>) -> PermissionDecision {
    let Some(principal) = ctx.principal.as_ref() else {
        return PermissionDecision::deny("principal is required");
    };

    principal_has_scope(principal, scope.as_ref())
}

pub fn has_project_scope(ctx: &RequestContext, scope: impl AsRef<str>) -> PermissionDecision {
    let Some(principal) = ctx.principal.as_ref() else {
        return PermissionDecision::deny("principal is required");
    };

    if let Some(decision) = bypass_decision(principal) {
        return decision;
    }

    if principal.is_owner {
        return PermissionDecision::allow(PermissionSource::Owner, "owner principal");
    }

    let Some(project_id) = ctx.project_id.as_deref() else {
        return PermissionDecision::deny("project context is required");
    };

    if let Some(principal_project_id) = principal.project_id.as_deref()
        && principal_project_id != project_id
    {
        return PermissionDecision::deny("principal project does not match request project");
    }

    principal_has_project_scope(principal, project_id, scope.as_ref())
}

fn principal_has_scope(principal: &Principal, scope: &str) -> PermissionDecision {
    if let Some(decision) = bypass_decision(principal) {
        return decision;
    }

    if principal.is_owner {
        return PermissionDecision::allow(PermissionSource::Owner, "owner principal");
    }

    let scopes = ScopeSet::from(&principal.scopes);

    if let Some(matched) = scopes.matched(scope) {
        return allow_matched(PermissionSource::DirectScope, matched);
    }

    if let Some(matched) = scopes.matched_system_role_scope(scope) {
        return allow_matched(PermissionSource::SystemRoleScope, matched);
    }

    // API-key-prefixed scopes are only authoritative for API key principals.
    if matches!(principal.kind, PrincipalKind::ApiKey)
        && let Some(matched) = scopes.matched_api_key_scope(scope)
    {
        return allow_matched(PermissionSource::ApiKeyScope, matched);
    }

    PermissionDecision::deny("required scope is missing")
}

fn principal_has_project_scope(
    principal: &Principal,
    project_id: &str,
    scope: &str,
) -> PermissionDecision {
    let scopes = ScopeSet::from(&principal.scopes);

    if let Some(matched) = scopes.matched_project_scope(project_id, scope) {
        return allow_matched(PermissionSource::DirectScope, matched);
    }

    if let Some(matched) = scopes.matched_system_role_scope(scope) {
        return allow_matched(PermissionSource::SystemRoleScope, matched);
    }

    if let Some(matched) = scopes.matched_project_membership_scope(project_id, scope) {
        return allow_matched(PermissionSource::ProjectMembershipScope, matched);
    }

    if let Some(matched) = scopes.matched_project_role_scope(project_id, scope) {
        return allow_matched(PermissionSource::ProjectRoleScope, matched);
    }

    // API keys must match the request project before their scopes are evaluated.
    if matches!(principal.kind, PrincipalKind::ApiKey)
        && let Some(matched) = scopes.matched_api_key_project_scope(project_id, scope)
    {
        return allow_matched(PermissionSource::ApiKeyScope, matched);
    }

    PermissionDecision::deny("required project scope is missing")
}

fn allow_matched(source: PermissionSource, matched: &crate::scopes::Scope) -> PermissionDecision {
    PermissionDecision::allow(source, format!("matched scope `{matched}`"))
}

fn bypass_decision(principal: &Principal) -> Option<PermissionDecision> {
    match principal.kind {
        PrincipalKind::System => Some(PermissionDecision::allow(
            PermissionSource::System,
            "system principal",
        )),
        PrincipalKind::Test => Some(PermissionDecision::allow(
            PermissionSource::Test,
            "test principal",
        )),
        PrincipalKind::User | PrincipalKind::ApiKey => None,
    }
}

/// Admin-GraphQL surface guard (S12 — pure logic mirror of Go
/// `internal/server/middleware/auth.go` surface separation).
///
/// The Go router (`internal/server/routes.go`) gates `/admin/graphql` behind
/// `WithJWTAuth` (user session) and `/openapi/v1/graphql` behind
/// `WithOpenAPIAuth`, which in turn only accepts `apikey.Type ==
/// TypeServiceAccount` (`auth.go:140-143`). A service-account API key may
/// therefore legitimately carry ordinary read/write scopes for the OpenAPI
/// surface — but it must NOT use those scopes to reach the admin GraphQL
/// surface, which is reserved for user principals.
///
/// This function returns `false` for service-account API-key principals
/// regardless of their scopes, mirroring the structural separation enforced by
/// Go's middleware chain. It is intentionally a pure boolean predicate so it
/// can be composed into any resolver-side guard without coupling to the HTTP
/// layer.
pub fn can_access_admin_graphql(principal: &Principal) -> bool {
    if matches!(principal.kind, PrincipalKind::ApiKey) {
        // Service-account keys are scoped to the OpenAPI surface only.
        // Ordinary user-key API keys are also blocked here: the admin GraphQL
        // surface is JWT-gated in Go, so the only API-key principal that ever
        // legitimately reaches a GraphQL resolver is one minted by
        // `WithOpenAPIAuth`, which is by construction service_account-typed.
        // We deny all api-key principals to be conservative; service-account
        // keys are the explicit contract case from RUST-P4-002 S12.
        if principal.api_key_kind.is_service_account() {
            return false;
        }
        // Non-service-account API keys: the Go admin surface requires a JWT
        // user, so an api-key principal on this surface is also a misuse.
        return false;
    }

    // System / Test / User principals are eligible; finer scope checks are
    // layered on top by `has_scope` / `has_project_scope`.
    matches!(
        principal.kind,
        PrincipalKind::System | PrincipalKind::Test | PrincipalKind::User
    )
}

/// Decision form of [`can_access_admin_graphql`], for audit logs. Carries the
/// exact reason so a resolver can surface a structured denial.
pub fn check_admin_graphql_access(principal: &Principal) -> PermissionDecision {
    if matches!(principal.kind, PrincipalKind::ApiKey) {
        return PermissionDecision::deny(
            "admin GraphQL is gated by user JWT; API-key principals (including \
             service-account) must use the OpenAPI GraphQL surface",
        );
    }

    PermissionDecision::allow(
        PermissionSource::DirectScope,
        "principal is eligible for admin GraphQL",
    )
}

#[cfg(test)]
mod tests {
    use crate::principal::Principal;
    use crate::scopes::{Scope, slug};

    use super::*;

    fn context_with(
        principal: Principal,
        project_id: Option<&str>,
    ) -> Result<RequestContext, crate::request_context::ContextConflictError> {
        let mut ctx = RequestContext::new();
        ctx.set_principal(principal)?;
        if let Some(project_id) = project_id {
            ctx.set_project_id(project_id)?;
        }
        Ok(ctx)
    }

    #[test]
    fn system_and_test_principals_are_allowed()
    -> Result<(), crate::request_context::ContextConflictError> {
        let system = context_with(Principal::system(), None)?;
        let test = context_with(Principal::test(), None)?;

        assert_eq!(
            has_scope(&system, slug::READ_CHANNELS).source(),
            Some(PermissionSource::System)
        );
        assert_eq!(
            has_project_scope(&system, slug::READ_CHANNELS).source(),
            Some(PermissionSource::System)
        );
        assert_eq!(
            has_scope(&test, slug::READ_CHANNELS).source(),
            Some(PermissionSource::Test)
        );
        assert_eq!(
            has_project_scope(&test, slug::READ_CHANNELS).source(),
            Some(PermissionSource::Test)
        );
        Ok(())
    }

    #[test]
    fn user_scope_allow_and_deny() -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(
            Principal::user("user-1").with_scope(slug::READ_CHANNELS),
            None,
        )?;

        let decision = has_scope(&ctx, slug::READ_CHANNELS);

        assert!(decision.is_allowed());
        assert_eq!(decision.source(), Some(PermissionSource::DirectScope));
        assert!(decision.reason().contains(slug::READ_CHANNELS));
        assert!(!has_scope(&ctx, slug::WRITE_CHANNELS).is_allowed());
        Ok(())
    }

    #[test]
    fn api_key_project_scope_allow_and_deny()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(
            Principal::api_key("key-1", "project-1").with_scope(slug::READ_CHANNELS),
            Some("project-1"),
        )?;

        assert!(has_project_scope(&ctx, slug::READ_CHANNELS).is_allowed());
        assert!(!has_project_scope(&ctx, slug::WRITE_CHANNELS).is_allowed());
        Ok(())
    }

    #[test]
    fn project_context_missing_denies_project_scope()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(
            Principal::api_key("key-1", "project-1").with_scope(slug::READ_CHANNELS),
            None,
        )?;

        let decision = has_project_scope(&ctx, slug::READ_CHANNELS);

        assert_eq!(
            decision,
            PermissionDecision::deny("project context is required")
        );
        Ok(())
    }

    #[test]
    fn project_id_mismatch_denies_project_scope()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(
            Principal::api_key("key-1", "project-1").with_scope(slug::READ_CHANNELS),
            Some("project-2"),
        )?;

        let decision = has_project_scope(&ctx, slug::READ_CHANNELS);

        assert_eq!(
            decision,
            PermissionDecision::deny("principal project does not match request project")
        );
        Ok(())
    }

    #[test]
    fn project_specific_scope_matches_only_same_project()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(
            Principal::user("user-1").with_scope(Scope::project("project-1", slug::READ_CHANNELS)),
            Some("project-1"),
        )?;

        assert!(has_project_scope(&ctx, slug::READ_CHANNELS).is_allowed());

        let mismatch = context_with(
            Principal::user("user-1").with_scope(Scope::project("project-1", slug::READ_CHANNELS)),
            Some("project-2"),
        )?;
        assert!(!has_project_scope(&mismatch, slug::READ_CHANNELS).is_allowed());
        Ok(())
    }

    #[test]
    fn owner_allows_before_project_context()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(Principal::user("owner-1").with_owner(true), None)?;

        let decision = has_project_scope(&ctx, slug::WRITE_PROJECTS);

        assert_eq!(decision.source(), Some(PermissionSource::Owner));
        Ok(())
    }

    #[test]
    fn system_role_scope_allows_after_direct_scope_check()
    -> Result<(), crate::request_context::ContextConflictError> {
        let direct = context_with(
            Principal::user("user-1")
                .with_scope(slug::READ_USERS)
                .with_scope(slug::SYSTEM_ADMIN),
            None,
        )?;
        let role = context_with(
            Principal::user("user-1").with_scope(Scope::system_role(slug::READ_USERS)),
            None,
        )?;

        assert_eq!(
            has_scope(&direct, slug::READ_USERS).source(),
            Some(PermissionSource::DirectScope)
        );
        assert_eq!(
            has_scope(&role, slug::READ_USERS).source(),
            Some(PermissionSource::SystemRoleScope)
        );
        Ok(())
    }

    #[test]
    fn project_sources_are_evaluated_in_declared_order()
    -> Result<(), crate::request_context::ContextConflictError> {
        let direct = context_with(
            Principal::user("user-1")
                .with_scope(Scope::project("project-1", slug::READ_PROJECTS))
                .with_scope(slug::SYSTEM_ADMIN)
                .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS))
                .with_scope(Scope::project_role("project-1", slug::READ_PROJECTS)),
            Some("project-1"),
        )?;
        let membership = context_with(
            Principal::user("user-1")
                .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS)),
            Some("project-1"),
        )?;
        let project_role = context_with(
            Principal::user("user-1")
                .with_scope(Scope::project_role("project-1", slug::READ_PROJECTS)),
            Some("project-1"),
        )?;

        assert_eq!(
            has_project_scope(&direct, slug::READ_PROJECTS).source(),
            Some(PermissionSource::DirectScope)
        );
        assert_eq!(
            has_project_scope(&membership, slug::READ_PROJECTS).source(),
            Some(PermissionSource::ProjectMembershipScope)
        );
        assert_eq!(
            has_project_scope(&project_role, slug::READ_PROJECTS).source(),
            Some(PermissionSource::ProjectRoleScope)
        );
        Ok(())
    }

    #[test]
    fn api_key_scopes_are_last_and_project_bound()
    -> Result<(), crate::request_context::ContextConflictError> {
        let api_key = context_with(
            Principal::api_key("key-1", "project-1")
                .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS)),
            Some("project-1"),
        )?;
        let user = context_with(
            Principal::user("user-1")
                .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS)),
            Some("project-1"),
        )?;

        assert_eq!(
            has_project_scope(&api_key, slug::READ_REQUESTS).source(),
            Some(PermissionSource::ApiKeyScope)
        );
        assert!(!has_project_scope(&user, slug::READ_REQUESTS).is_allowed());
        Ok(())
    }

    // -----------------------------------------------------------------
    // S12: OpenAPI service-account admin-GraphQL restriction.
    //
    // Mirrors Go `internal/server/middleware/auth.go:140-143` (only
    // service-account API keys reach `/openapi/v1/graphql`) combined with
    // `internal/server/routes.go:96-104` (admin GraphQL is JWT-gated by
    // `WithJWTAuth`). A service-account key carrying ordinary read/write
    // scopes for the OpenAPI surface must still NOT reach admin GraphQL.
    // -----------------------------------------------------------------

    #[test]
    fn service_account_api_key_cannot_access_admin_graphql_even_with_scopes() {
        // Service-account key that legitimately holds OpenAPI-surface scopes.
        let principal = Principal::api_key_service_account("svc-1", "project-1")
            .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS))
            .with_scope(Scope::api_key_project("project-1", slug::WRITE_REQUESTS));

        assert!(!can_access_admin_graphql(&principal));
        let decision = check_admin_graphql_access(&principal);
        assert!(!decision.is_allowed());
        assert!(decision.source().is_none());
        assert!(decision.reason().contains("admin GraphQL"));
    }

    #[test]
    fn user_principal_can_access_admin_graphql() {
        let principal = Principal::user("user-1").with_scope(slug::READ_USERS);
        assert!(can_access_admin_graphql(&principal));

        let decision = check_admin_graphql_access(&principal);
        assert!(decision.is_allowed());
    }

    #[test]
    fn system_and_test_principals_can_access_admin_graphql() {
        assert!(can_access_admin_graphql(&Principal::system()));
        assert!(can_access_admin_graphql(&Principal::test()));
    }

    #[test]
    fn ordinary_api_key_also_denied_admin_graphql() {
        // Conservative: the admin surface is JWT-gated in Go, so any api-key
        // principal there is a misuse. Documented as part of S12.
        let principal = Principal::api_key("key-1", "project-1");
        assert!(!can_access_admin_graphql(&principal));
    }

    // -----------------------------------------------------------------
    // S13: policy rule coverage — the five rule classes mirrored from Go
    // `internal/scopes/*_test.go` golden intent. Each fixture asserts the
    // allow/deny boundary AND the `PermissionDecision` source/reason so the
    // RBAC decision chain stays auditable.
    // -----------------------------------------------------------------

    // Rule class 1 — user-owned: a user principal with a direct (global)
    // scope is allowed; lacking the scope it is denied. Mirrors Go
    // `rule_user_scope_test.go::TestCheckUserPermission` "user with direct
    // scope" / "user without required scope" cases.
    #[test]
    fn policy_user_owned_direct_scope_rule()
    -> Result<(), crate::request_context::ContextConflictError> {
        let allowed = context_with(Principal::user("user-1").with_scope(slug::READ_USERS), None)?;
        let denied = context_with(Principal::user("user-2"), None)?;

        let allow = has_scope(&allowed, slug::READ_USERS);
        assert!(allow.is_allowed());
        assert_eq!(allow.source(), Some(PermissionSource::DirectScope));
        assert!(allow.reason().contains(slug::READ_USERS));

        let deny = has_scope(&denied, slug::READ_USERS);
        assert!(!deny.is_allowed());
        assert_eq!(deny.source(), None);
        assert_eq!(deny.reason(), "required scope is missing");
        Ok(())
    }

    // Rule class 2 — user-project: project membership scopes are evaluated
    // only when the project context matches. Mirrors Go
    // `rule_user_project_scope.go` — `userHasProjectScope` walks
    // membership / role edges, gated by the request project id.
    #[test]
    fn policy_user_project_membership_rule()
    -> Result<(), crate::request_context::ContextConflictError> {
        let same_project = context_with(
            Principal::user("user-1")
                .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS)),
            Some("project-1"),
        )?;
        let other_project = context_with(
            Principal::user("user-1")
                .with_scope(Scope::project_membership("project-1", slug::READ_PROJECTS)),
            Some("project-2"),
        )?;

        let allow = has_project_scope(&same_project, slug::READ_PROJECTS);
        assert!(allow.is_allowed());
        assert_eq!(
            allow.source(),
            Some(PermissionSource::ProjectMembershipScope)
        );

        let deny = has_project_scope(&other_project, slug::READ_PROJECTS);
        assert!(!deny.is_allowed());
        Ok(())
    }

    // Rule class 3 — api-key-project: API-key project scopes are evaluated
    // last, only for api-key principals, and only when the project matches.
    // Mirrors Go `rule_apikey_scope_test.go::TestAPIKeyProjectScopeReadRule`.
    #[test]
    fn policy_api_key_project_scope_rule()
    -> Result<(), crate::request_context::ContextConflictError> {
        let allow = context_with(
            Principal::api_key("key-1", "project-1")
                .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS)),
            Some("project-1"),
        )?;
        let missing_scope =
            context_with(Principal::api_key("key-1", "project-1"), Some("project-1"))?;
        let wrong_project = context_with(
            Principal::api_key("key-1", "project-1")
                .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS)),
            Some("project-2"),
        )?;

        let allowed = has_project_scope(&allow, slug::READ_REQUESTS);
        assert!(allowed.is_allowed());
        assert_eq!(allowed.source(), Some(PermissionSource::ApiKeyScope));

        assert!(!has_project_scope(&missing_scope, slug::READ_REQUESTS).is_allowed());
        // Wrong project is rejected before scope evaluation by the principal
        // project-id check.
        let wrong = has_project_scope(&wrong_project, slug::READ_REQUESTS);
        assert!(!wrong.is_allowed());
        assert_eq!(
            wrong.reason(),
            "principal project does not match request project"
        );
        Ok(())
    }

    // Rule class 4 — owner bypass: owner principals are allowed before any
    // project-context or scope check, mirroring Go `userHasSystemScope` /
    // `userHasProjectScope` short-circuiting on `user.IsOwner`.
    #[test]
    fn policy_owner_bypass_rule() -> Result<(), crate::request_context::ContextConflictError> {
        let owner = context_with(Principal::user("owner-1").with_owner(true), None)?;

        let decision = has_project_scope(&owner, slug::WRITE_PROJECTS);
        assert!(decision.is_allowed());
        assert_eq!(decision.source(), Some(PermissionSource::Owner));
        assert_eq!(decision.reason(), "owner principal");
        Ok(())
    }

    // Rule class 5 — deny-by-default: a user principal without the required
    // scope (direct, role, or project) is denied with the canonical reason.
    // Mirrors Go `policy.go::QueryPolicy.EvalQuery` returning
    // `privacy.Denyf("default deny")` after every rule Skips.
    #[test]
    fn policy_deny_by_default_rule() -> Result<(), crate::request_context::ContextConflictError> {
        let bare = context_with(Principal::user("user-1"), None)?;

        let decision = has_scope(&bare, slug::READ_USERS);
        assert!(!decision.is_allowed());
        assert_eq!(decision.source(), None);
        assert_eq!(decision.reason(), "required scope is missing");

        let project_decision = has_project_scope(&bare, slug::READ_PROJECTS);
        assert!(!project_decision.is_allowed());
        Ok(())
    }

    // Cross-cut: api-key project scope must NOT be granted to a user
    // principal even if it carries the api-key scope string. Mirrors Go
    // `rule_apikey_scope.go::apiKeyQueryRule.EvalQuery` resolving the
    // principal from the API-key context only.
    #[test]
    fn policy_api_key_scopes_do_not_apply_to_user_principals()
    -> Result<(), crate::request_context::ContextConflictError> {
        let user_with_api_key_scope = context_with(
            Principal::user("user-1")
                .with_scope(Scope::api_key_project("project-1", slug::READ_REQUESTS)),
            Some("project-1"),
        )?;

        let decision = has_project_scope(&user_with_api_key_scope, slug::READ_REQUESTS);
        assert!(!decision.is_allowed());
        Ok(())
    }

    // Rule class 1b — user-system-role: a user principal whose scope comes from
    // a system role (no direct scope) is allowed via the SystemRoleScope path.
    // Mirrors Go `rule_test.go::TestCheckUserPermission` "user with role scope"
    // (L216-233): user with empty direct `Scopes` but a role carrying
    // "read_users" -> true. Fills the last TestCheckUserPermission gap (owner is
    // covered by `policy_owner_bypass_rule`, direct+without by
    // `policy_user_owned_direct_scope_rule`).
    #[test]
    fn policy_user_system_role_scope_rule()
    -> Result<(), crate::request_context::ContextConflictError> {
        let role_scope = context_with(
            Principal::user("user-1").with_scope(Scope::system_role(slug::READ_USERS)),
            None,
        )?;

        let allow = has_scope(&role_scope, slug::READ_USERS);
        assert!(allow.is_allowed());
        assert_eq!(allow.source(), Some(PermissionSource::SystemRoleScope));
        Ok(())
    }

    // ====================================================================
    // Go `internal/scopes/{rule_test,policy_test}.go` — STRUCTURAL-GAP
    // catalogue (pending generic ent-privacy port).
    //
    // The remaining Go tests in these two files exercise ent-privacy constructs
    // with no direct Rust analogue:
    //   * `policy_test.go` — `QueryPolicy`/`MutationPolicy`/`Policy` are SLICES
    //     of `privacy.QueryRule`/`privacy.MutationRule` evaluated as a generic
    //     first-decision-wins chain with Allow/Deny/Skip/nil/custom-error
    //     semantics over MOCK rules (`mockQueryRule`/`mockMutationRule`).
    //     Rust uses a CONCRETE rbac evaluator (`has_scope`/`has_project_scope` +
    //     `principal_has_scope` above). The semantic intent (default-deny,
    //     first-decision-wins, skip-falls-through, owner-short-circuit) IS
    //     covered by `policy_deny_by_default_rule` / `policy_owner_bypass_rule`
    //     / the rule-class tests; the generic mock-rule SHAPE is not ported.
    //   * `rule_test.go::TestAlwaysDeny` — `AlwaysDeny()` returns a
    //     `privacy.QueryRule` that always denies. Rust has no named
    //     always-deny rule construct; the semantic is `policy_deny_by_default_rule`.
    //   * `rule_test.go::TestGetUserFromContext`/`TestGetAPIKeyFromContext` —
    //     extract `*ent.User`/`*ent.APIKey` from `context.Context`. Rust models
    //     identity as a `Principal` inside `RequestContext`, not an ent entity
    //     stashed in a Go context; the extraction seam diverges structurally.
    // These are pinned `#[ignore]` so the parity gap is auditable; they require
    // a generic privacy-Policy / ent-context port to express faithfully.
    // ====================================================================
    #[test]
    #[ignore = "structural gap: Go generic ent-privacy Policy/QueryPolicy/MutationPolicy slice + AlwaysDeny rule + ent-context extraction vs Rust concrete rbac evaluator + Principal/RequestContext"]
    fn go_scopes_rule_and_policy_tests_pending_structural_gap_catalogue() {
        // policy_test.go: TestQueryPolicy_EvalQuery (L32-130, 10 subtests),
        // TestMutationPolicy_EvalMutation (L132-230, 10 subtests),
        // TestPolicy_Structure (L232-256), TestPolicy_EvalQuery (L258-347,
        // 7 subtests), TestPolicy_EvalMutation (L349-438, 7 subtests),
        // TestPolicy_Complete (L440-462).
        // rule_test.go: TestAlwaysDeny (L14-52, 3 subtests),
        // TestGetUserFromContext (L264-311, 3 subtests),
        // TestGetAPIKeyFromContext (L313-360, 3 subtests).
        // Pure-logic subsets already covered: TestHasScope (scopes.rs),
        // TestHasRoleScope (scopes.rs), TestCheckUserPermission (rbac.rs
        // owner/direct/role/without tests above).
    }

    // ====================================================================
    // Go `internal/authz/scope_test.go` parity (L1-346).
    //
    // Go scope tests exercise `HasScope(ctx, scope)` which resolves the
    // principal from context, then checks ent.User/ent.APIKey edges for
    // scopes. Most cases are DB-backed (ent.User with Scopes/Roles/
    // ProjectUsers edges). Rust flattens these into Principal.scopes +
    // ScopeSet at construction time; the pure-logic checks are covered by
    // the rule-class tests above. The additions below make the Go mapping
    // explicit for the non-DB-backed cases and catalogue the structural
    // gaps.
    // ====================================================================

    /// Mirrors Go `TestHasScope_SystemPrincipal` (scope_test.go:14-24):
    /// system principal has all scopes, including read_channels and
    /// write_settings. Behaviorally covered by
    /// `system_and_test_principals_are_allowed`; this test makes the Go
    /// citation explicit.
    #[test]
    fn go_has_scope_system_principal_has_all_scopes()
    -> Result<(), crate::request_context::ContextConflictError> {
        let ctx = context_with(Principal::system(), None)?;

        assert!(has_scope(&ctx, slug::READ_CHANNELS).is_allowed());
        assert!(has_scope(&ctx, slug::WRITE_SETTINGS).is_allowed());
        Ok(())
    }

    /// Mirrors Go `TestHasScope_NoPrincipal` (scope_test.go:26-32): no
    /// principal in context -> HasScope returns false for any scope.
    #[test]
    fn go_has_scope_no_principal_denies() {
        let ctx = RequestContext::new();

        let decision = has_scope(&ctx, slug::READ_CHANNELS);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), "principal is required");
    }

    /// Mirrors Go `TestRequireScope_Pass` (scope_test.go:320-326) and
    /// `TestRequireScope_NoPrincipal` (L340-346).
    ///
    /// Go: `RequireScope` returns error when `!HasScope` (scope.go:59-66).
    /// Rust: `has_scope` returns a Deny decision. `TestRequireScope_Fail`
    /// (L328-338) is DB-backed (ent.User) — see structural-gap catalogue
    /// below.
    #[test]
    fn go_require_scope_pass_and_no_principal()
    -> Result<(), crate::request_context::ContextConflictError> {
        // System principal -> RequireScope passes (Go L320-326).
        let system_ctx = context_with(Principal::system(), None)?;
        assert!(has_scope(&system_ctx, slug::READ_CHANNELS).is_allowed());

        // No principal -> RequireScope fails (Go L340-346).
        let no_principal_ctx = RequestContext::new();
        let decision = has_scope(&no_principal_ctx, slug::READ_CHANNELS);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), "principal is required");
        Ok(())
    }

    /// Structural-gap catalogue for Go `scope_test.go` tests that are
    /// DB-backed (ent.User/ent.APIKey with Scopes/Roles/ProjectUsers edges)
    /// or use ent `privacy.DecisionContext` — neither has a direct Rust
    /// equivalent. The pure-logic intent of each rule class IS covered by
    /// the rbac/policy tests above; the ent-entity-resolution shape diverges.
    #[test]
    #[ignore = "structural gap: Go ent.User/APIKey edge traversal + privacy.DecisionContext vs Rust flattened Principal.scopes/ScopeSet"]
    fn go_scope_test_db_backed_structural_gap_catalogue() {
        // DB-backed ent.User scope tests (require contexts.WithUser +
        //   ent.User edges):
        // TestHasScope_UserPrincipal_Owner (L34-48): user.IsOwner -> true.
        //   Rust: covered by policy_owner_bypass_rule.
        // TestHasScope_UserPrincipal_DirectScope (L50-64): user.Scopes.
        //   Rust: covered by policy_user_owned_direct_scope_rule.
        // TestHasScope_UserPrincipal_RoleScope (L66-87): role.Scopes.
        //   Rust: covered by policy_user_system_role_scope_rule.
        // TestHasScope_UserPrincipal_NoUser (L89-95): no ent.User -> false.
        //   Rust: covered by policy_deny_by_default_rule.
        // TestHasScope_UserPrincipal_ProjectMembershipScope (L97-120).
        //   Rust: covered by policy_user_project_membership_rule.
        // TestHasScope_UserPrincipal_ProjectRoleScope (L122-146).
        //   Rust: covered by policy_api_key_project_scope_rule (analogous).
        // TestHasScope_UserPrincipal_ProjectRoleScopeRequiresMembership
        //   (L148-167). Rust: project_filter tests cover the intent.
        // TestHasScope_UserPrincipal_ProjectScope_WrongProject (L169-188).
        //   Rust: covered by project_id_mismatch_denies_project_scope.
        // TestHasScope_UserPrincipal_ProjectScope_NoProjectInContext
        //   (L190-208). Rust: covered by
        //   project_context_missing_denies_project_scope.
        // TestHasScope_UserPrincipal_ProjectMembershipOwner (L210-229).
        //   Rust: covered by policy_owner_bypass_rule.
        //
        // DB-backed ent.APIKey scope tests:
        // TestHasScope_APIKeyPrincipal (L231-249): apiKey.Scopes.
        //   Rust: covered by policy_api_key_project_scope_rule.
        // TestHasScope_APIKeyPrincipal_NoAPIKey (L251-257): no ent.APIKey.
        //   Rust: covered by policy_deny_by_default_rule.
        //
        // privacy.DecisionContext tests (ent-privacy construct):
        // TestWithScopeDecision_Allow/Deny/NoPrincipal (L259-288).
        // TestRunWithScopeDecision (L290-310).
        // TestRunWithScopeDecision_ScopeIsolation (L312-318).
        // TestRequireScope_Fail (L328-338): DB-backed ent.User.
    }
}

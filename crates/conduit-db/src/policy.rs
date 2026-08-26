use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    System,
    Test,
    User,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub subject_id: String,
    pub project_scope: ProjectScope,
    /// Go `user.IsOwner`. The owner rule (`scopes.OwnerRule`,
    /// `internal/scopes/rule_owner.go`) short-circuits every entity policy with
    /// `privacy.Allow`, so this bypasses all scope checks.
    pub is_owner: bool,
    /// The principal's effective scope slugs — Go's `user.Scopes` plus the
    /// role-expanded system-role scopes (`userHasSystemScope`,
    /// `rule_user_scope.go:47-59`). Empty means "no scopes", which under Go's
    /// `default deny` (`internal/scopes/policy.go`) denies every guarded read
    /// and write.
    pub scope_slugs: BTreeSet<String>,
}

/// Wildcard scope granting every permission.
///
/// Go has no wildcard rule (owner authority flows through `user.IsOwner`), but
/// the Rust `initialize` seeds the owner user AND the built-in `Admin` role with
/// `scopes = ["*"]` (`system_service.rs`). Honouring it here is required for
/// parity with that seed — otherwise every Admin-role (non-owner) user would be
/// denied.
pub const WILDCARD_SCOPE: &str = "*";

impl Principal {
    pub fn system() -> Self {
        Self {
            kind: PrincipalKind::System,
            subject_id: "system".to_string(),
            project_scope: ProjectScope::All,
            is_owner: true,
            scope_slugs: BTreeSet::new(),
        }
    }

    pub fn test() -> Self {
        Self {
            kind: PrincipalKind::Test,
            subject_id: "test".to_string(),
            project_scope: ProjectScope::All,
            is_owner: true,
            scope_slugs: BTreeSet::new(),
        }
    }

    pub fn user(subject_id: impl Into<String>, project_scope: ProjectScope) -> Self {
        Self {
            kind: PrincipalKind::User,
            subject_id: subject_id.into(),
            project_scope,
            is_owner: false,
            scope_slugs: BTreeSet::new(),
        }
    }

    pub fn api_key(subject_id: impl Into<String>, project_scope: ProjectScope) -> Self {
        Self {
            kind: PrincipalKind::ApiKey,
            subject_id: subject_id.into(),
            project_scope,
            is_owner: false,
            scope_slugs: BTreeSet::new(),
        }
    }

    /// Mark this principal as an owner (Go `user.IsOwner`). The owner rule
    /// short-circuits every scope check (`scopes.OwnerRule`, `rule_owner.go`).
    pub fn with_owner(mut self, is_owner: bool) -> Self {
        self.is_owner = is_owner;
        self
    }

    /// Attach the effective scope slugs (Go: the user's direct `Scopes` plus the
    /// slugs expanded from their system roles — `userHasSystemScope`).
    pub fn with_scope_slugs<I, S>(mut self, slugs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scope_slugs = slugs.into_iter().map(Into::into).collect();
        self
    }

    /// Does this principal hold `scope`?
    ///
    /// Mirrors Go `userHasSystemScope` (`internal/scopes/rule.go:46-60`):
    /// owner short-circuits, then the direct scope set is consulted. The
    /// [`WILDCARD_SCOPE`] check has no Go counterpart but is required by the
    /// Rust `initialize` seed (see that constant's docs).
    pub fn has_scope(&self, scope: &str) -> bool {
        if self.is_owner {
            return true;
        }
        self.scope_slugs.contains(WILDCARD_SCOPE) || self.scope_slugs.contains(scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    All,
    Owner,
    Projects(BTreeSet<String>),
    None,
}

impl ProjectScope {
    pub fn project_ids<const N: usize>(ids: [&str; N]) -> Self {
        Self::Projects(ids.into_iter().map(ToString::to_string).collect())
    }

    fn contains(&self, project_id: &str) -> bool {
        match self {
            Self::All | Self::Owner => true,
            Self::Projects(projects) => projects.contains(project_id),
            Self::None => false,
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::None)
            || matches!(self, Self::Projects(projects) if projects.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyContext {
    pub principal: Option<Principal>,
    bypass: bool,
}

impl PolicyContext {
    pub const fn anonymous() -> Self {
        Self {
            principal: None,
            bypass: false,
        }
    }

    pub fn new(principal: Principal) -> Self {
        Self {
            principal: Some(principal),
            bypass: false,
        }
    }

    pub const fn is_bypassed(&self) -> bool {
        self.bypass
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
}

impl PolicyDecision {
    pub fn into_result(self) -> Result<(), PolicyError> {
        match self {
            Self::Allow => Ok(()),
            Self::Deny { reason } => Err(PolicyError::Denied(reason)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectAccess {
    Read,
    Write,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("policy denied: {0}")]
    Denied(String),
}

pub fn require_principal(context: &PolicyContext) -> PolicyDecision {
    if context.bypass {
        return PolicyDecision::Allow;
    }

    match context.principal.as_ref() {
        Some(principal)
            if matches!(principal.kind, PrincipalKind::System | PrincipalKind::Test) =>
        {
            PolicyDecision::Allow
        }
        Some(_) => PolicyDecision::Allow,
        None => PolicyDecision::Deny {
            reason: "principal is required".to_string(),
        },
    }
}

pub fn require_project_access(
    context: &PolicyContext,
    project_id: &str,
    _access: ProjectAccess,
) -> PolicyDecision {
    if context.bypass {
        return PolicyDecision::Allow;
    }

    let Some(principal) = context.principal.as_ref() else {
        return PolicyDecision::Deny {
            reason: "principal is required".to_string(),
        };
    };

    match principal.kind {
        PrincipalKind::System | PrincipalKind::Test => PolicyDecision::Allow,
        PrincipalKind::User => {
            if principal.project_scope.contains(project_id) {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny {
                    reason: format!("user cannot access project {project_id}"),
                }
            }
        }
        PrincipalKind::ApiKey => {
            if principal.project_scope.is_empty() {
                return PolicyDecision::Deny {
                    reason: "api key must include at least one project scope".to_string(),
                };
            }
            if principal.project_scope.contains(project_id) {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny {
                    reason: format!("api key cannot access project {project_id}"),
                }
            }
        }
    }
}

/// Scope gate for an entity operation — the Rust counterpart of one Go ent
/// `Policy()` rule chain.
///
/// Go models this per entity (`internal/ent/schema/*.go`) as an ordered list of
/// rules evaluated by `scopes.QueryPolicy::EvalQuery` /
/// `MutationPolicy::EvalMutation` (`internal/scopes/policy.go`), which — unlike
/// upstream ent — **defaults to `privacy.Denyf("default deny")`** when no rule
/// allows. The rules that matter for the admin surface reduce to:
///
/// * `OwnerRule` — `user.IsOwner` allows everything (`rule_owner.go`);
/// * `UserReadScopeRule(slug)` / `UserWriteScopeRule(slug)` — the user must hold
///   the slug directly or through a system role (`userHasSystemScope`);
/// * `APIKeyScopeQueryRule(slug)` — the same check for API-key principals.
///
/// `System`/`Test` principals bypass, matching `RunWithSystemBypass`
/// (`authz/bypass.go`) and the pre-existing behaviour of the sibling guards.
///
/// The caller supplies the slug for the entity+operation being performed; see
/// [`crate::repo`] guards for the call sites.
pub fn require_scope(context: &PolicyContext, scope: &str) -> PolicyDecision {
    if context.bypass {
        return PolicyDecision::Allow;
    }

    let Some(principal) = context.principal.as_ref() else {
        // Go: `getUserFromContext` fails -> `privacy.Skipf` -> chain falls
        // through to the policy's default deny.
        return PolicyDecision::Deny {
            reason: "principal is required".to_string(),
        };
    };

    match principal.kind {
        PrincipalKind::System | PrincipalKind::Test => PolicyDecision::Allow,
        PrincipalKind::User | PrincipalKind::ApiKey => {
            if principal.has_scope(scope) {
                PolicyDecision::Allow
            } else {
                // Go's `default deny` (`scopes/policy.go`).
                PolicyDecision::Deny {
                    reason: format!("required scope `{scope}` is missing"),
                }
            }
        }
    }
}

/// Scope slugs, mirroring Go `internal/scopes/scopes.go` verbatim.
///
/// Duplicated here (rather than imported from `conduit-auth`) because
/// `conduit-db` deliberately does not depend on that crate. The string values
/// are the contract — they must stay byte-identical to Go's `ScopeSlug` consts
/// and to `conduit_auth::scopes::slug`.
pub mod scope_slug {
    pub const READ_DASHBOARD: &str = "read_dashboard";
    pub const READ_CHANNELS: &str = "read_channels";
    pub const WRITE_CHANNELS: &str = "write_channels";
    pub const READ_DATA_STORAGES: &str = "read_data_storages";
    pub const WRITE_DATA_STORAGES: &str = "write_data_storages";
    pub const READ_USERS: &str = "read_users";
    pub const WRITE_USERS: &str = "write_users";
    pub const READ_SETTINGS: &str = "read_settings";
    pub const WRITE_SETTINGS: &str = "write_settings";
    pub const READ_ROLES: &str = "read_roles";
    pub const WRITE_ROLES: &str = "write_roles";
    pub const READ_PROJECTS: &str = "read_projects";
    pub const WRITE_PROJECTS: &str = "write_projects";
    pub const READ_API_KEYS: &str = "read_api_keys";
    pub const WRITE_API_KEYS: &str = "write_api_keys";
    pub const READ_REQUESTS: &str = "read_requests";
    pub const WRITE_REQUESTS: &str = "write_requests";
    pub const READ_PROMPTS: &str = "read_prompts";
    pub const WRITE_PROMPTS: &str = "write_prompts";
    pub const READ_GROUPS: &str = "read_groups";
    pub const WRITE_GROUPS: &str = "write_groups";
    pub const READ_SUBSCRIPTIONS: &str = "read_subscriptions";
    pub const WRITE_SUBSCRIPTIONS: &str = "write_subscriptions";
    pub const READ_BILLING: &str = "read_billing";
    pub const WRITE_BILLING: &str = "write_billing";
    pub const GRANT_CREDIT: &str = "grant_credit";
    pub const READ_COMMERCIALIZATION: &str = "read_commercialization";
    pub const WRITE_COMMERCIALIZATION: &str = "write_commercialization";
}

/// The entities carrying an ent `Policy()` in Go, one variant per
/// `internal/ent/schema/*.go` that declares one.
///
/// Go attaches the rule chain to the *entity*, so the required scope is a
/// property of (entity, read|write) — not of the calling resolver. This enum is
/// that lookup table, so a repo guard names its entity and gets the Go scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyEntity {
    ApiKey,
    ApiKeyProfileTemplate,
    Channel,
    ChannelModelPrice,
    ChannelModelPriceVersion,
    DataStorage,
    Model,
    OidcIdentity,
    Project,
    Prompt,
    PromptProtectionRule,
    Request,
    Role,
    System,
    Thread,
    Trace,
    UsageLog,
    User,
    UserProject,
}

impl PolicyEntity {
    /// The scope Go's rule chain requires for `access` on this entity.
    ///
    /// Sourced one-for-one from each schema's `Policy()` body — e.g.
    /// `Channel.Policy()` uses `ScopeReadChannels` / `ScopeWriteChannels`
    /// (`internal/ent/schema/channel.go`), and the price/version/model/
    /// prompt-protection entities intentionally reuse the *channel* scopes.
    pub const fn required_scope(self, access: ProjectAccess) -> &'static str {
        match (self, access) {
            (Self::ApiKey | Self::ApiKeyProfileTemplate, ProjectAccess::Read) => {
                scope_slug::READ_API_KEYS
            }
            (Self::ApiKey | Self::ApiKeyProfileTemplate, ProjectAccess::Write) => {
                scope_slug::WRITE_API_KEYS
            }
            // channel.go / channel_model_price.go / channel_model_price_versions.go /
            // model.go / prompt_protection_rule.go all gate on the channel scopes.
            (
                Self::Channel
                | Self::ChannelModelPrice
                | Self::ChannelModelPriceVersion
                | Self::Model
                | Self::PromptProtectionRule,
                ProjectAccess::Read,
            ) => scope_slug::READ_CHANNELS,
            (
                Self::Channel
                | Self::ChannelModelPrice
                | Self::ChannelModelPriceVersion
                | Self::Model
                | Self::PromptProtectionRule,
                ProjectAccess::Write,
            ) => scope_slug::WRITE_CHANNELS,
            (Self::DataStorage, ProjectAccess::Read) => scope_slug::READ_DATA_STORAGES,
            (Self::DataStorage, ProjectAccess::Write) => scope_slug::WRITE_DATA_STORAGES,
            // oidc_identity.go / user.go / user_project.go gate on user scopes.
            (Self::OidcIdentity | Self::User | Self::UserProject, ProjectAccess::Read) => {
                scope_slug::READ_USERS
            }
            (Self::OidcIdentity | Self::User | Self::UserProject, ProjectAccess::Write) => {
                scope_slug::WRITE_USERS
            }
            (Self::Project, ProjectAccess::Read) => scope_slug::READ_PROJECTS,
            (Self::Project, ProjectAccess::Write) => scope_slug::WRITE_PROJECTS,
            (Self::Prompt, ProjectAccess::Read) => scope_slug::READ_PROMPTS,
            (Self::Prompt, ProjectAccess::Write) => scope_slug::WRITE_PROMPTS,
            // request.go / thread.go / trace.go / usage_log.go gate on request scopes.
            (Self::Request | Self::Thread | Self::Trace | Self::UsageLog, ProjectAccess::Read) => {
                scope_slug::READ_REQUESTS
            }
            (Self::Request | Self::Thread | Self::Trace | Self::UsageLog, ProjectAccess::Write) => {
                scope_slug::WRITE_REQUESTS
            }
            (Self::Role, ProjectAccess::Read) => scope_slug::READ_ROLES,
            (Self::Role, ProjectAccess::Write) => scope_slug::WRITE_ROLES,
            (Self::System, ProjectAccess::Read) => scope_slug::READ_SETTINGS,
            (Self::System, ProjectAccess::Write) => scope_slug::WRITE_SETTINGS,
        }
    }
}

/// Evaluate an entity's Go rule chain: scope check, then project isolation.
///
/// Mirrors the composition every Go `Policy()` declares — a scope rule
/// (`UserReadScopeRule` / `UserProjectScopeReadRule` / the API-key variants)
/// plus `OwnerRule`, with `scopes.QueryPolicy::EvalQuery` supplying the
/// `privacy.Denyf("default deny")` fallback (`internal/scopes/policy.go`).
///
/// `project_id` is `None` for the globally-scoped entities (Go's
/// `UserReadScopeRule` chains, which carry no project filter — `System`,
/// `User`, `Role`, `Channel`, …); pass `Some` for the project-scoped ones so the
/// existing [`require_project_access`] isolation also applies.
pub fn require_entity_access(
    context: &PolicyContext,
    entity: PolicyEntity,
    access: ProjectAccess,
    project_id: Option<&str>,
) -> PolicyDecision {
    let scope_decision = require_scope(context, entity.required_scope(access));
    if let PolicyDecision::Deny { .. } = scope_decision {
        return scope_decision;
    }
    match project_id {
        Some(project_id) => require_project_access(context, project_id, access),
        None => PolicyDecision::Allow,
    }
}

pub fn with_bypass<T>(context: &mut PolicyContext, f: impl FnOnce(&mut PolicyContext) -> T) -> T {
    let previous = context.bypass;
    context.bypass = true;
    let result = f(context);
    context.bypass = previous;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_principal_is_denied() {
        let context = PolicyContext::anonymous();

        assert_eq!(
            require_principal(&context),
            PolicyDecision::Deny {
                reason: "principal is required".to_string()
            }
        );
        assert!(
            require_project_access(&context, "project-a", ProjectAccess::Read)
                .into_result()
                .is_err()
        );
    }

    #[test]
    fn owner_scope_allows_any_project() {
        let context = PolicyContext::new(Principal::user("user-1", ProjectScope::Owner));

        assert_eq!(
            require_project_access(&context, "project-a", ProjectAccess::Write),
            PolicyDecision::Allow
        );
        assert_eq!(
            require_project_access(&context, "project-b", ProjectAccess::Read),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn project_member_is_limited_to_listed_project() {
        let context = PolicyContext::new(Principal::user(
            "user-1",
            ProjectScope::project_ids(["project-a"]),
        ));

        assert_eq!(
            require_project_access(&context, "project-a", ProjectAccess::Read),
            PolicyDecision::Allow
        );
        assert_eq!(
            require_project_access(&context, "project-b", ProjectAccess::Read),
            PolicyDecision::Deny {
                reason: "user cannot access project project-b".to_string()
            }
        );
    }

    #[test]
    fn api_key_is_limited_and_requires_scope() {
        let scoped = PolicyContext::new(Principal::api_key(
            "key-1",
            ProjectScope::project_ids(["project-a"]),
        ));
        let unscoped = PolicyContext::new(Principal::api_key("key-2", ProjectScope::None));

        assert_eq!(
            require_project_access(&scoped, "project-a", ProjectAccess::Read),
            PolicyDecision::Allow
        );
        assert_eq!(
            require_project_access(&scoped, "project-b", ProjectAccess::Read),
            PolicyDecision::Deny {
                reason: "api key cannot access project project-b".to_string()
            }
        );
        assert_eq!(
            require_project_access(&unscoped, "project-a", ProjectAccess::Read),
            PolicyDecision::Deny {
                reason: "api key must include at least one project scope".to_string()
            }
        );
    }

    #[test]
    fn bypass_does_not_leak_outside_closure() {
        let mut context = PolicyContext::anonymous();

        let inside = with_bypass(&mut context, |context| {
            assert!(context.is_bypassed());
            require_project_access(context, "project-a", ProjectAccess::Write)
        });

        assert_eq!(inside, PolicyDecision::Allow);
        assert!(!context.is_bypassed());
        assert_eq!(
            require_project_access(&context, "project-a", ProjectAccess::Write),
            PolicyDecision::Deny {
                reason: "principal is required".to_string()
            }
        );
    }

    // =======================================================================
    // Scope rules (Go `internal/scopes/rule.go` + each entity's `Policy()`)
    // =======================================================================

    /// Go `userHasSystemScope` (`rule.go:52-60`): the owner short-circuits every
    /// scope check regardless of the slug set, via `scopes.OwnerRule`.
    #[test]
    fn owner_principal_satisfies_every_scope() {
        let owner = Principal::user("1", ProjectScope::Owner).with_owner(true);
        assert!(owner.has_scope(scope_slug::WRITE_CHANNELS));
        assert!(owner.has_scope(scope_slug::WRITE_SETTINGS));
        assert!(owner.has_scope("some_scope_that_does_not_exist"));
    }

    /// A non-owner holds exactly the slugs granted — nothing more. This is the
    /// behaviour the bypass principal used to erase.
    #[test]
    fn member_principal_holds_only_granted_scopes() {
        let member = Principal::user("7", ProjectScope::project_ids(["p1"]))
            .with_scope_slugs([scope_slug::READ_CHANNELS]);
        assert!(member.has_scope(scope_slug::READ_CHANNELS));
        assert!(!member.has_scope(scope_slug::WRITE_CHANNELS));
        assert!(!member.has_scope(scope_slug::READ_SETTINGS));
    }

    /// The Rust `initialize` seed gives the owner user and the built-in `Admin`
    /// role `scopes = ["*"]`, so the wildcard must grant everything or every
    /// Admin-role (non-owner) user would be locked out. No Go counterpart.
    #[test]
    fn wildcard_scope_grants_all() {
        let admin = Principal::user("9", ProjectScope::All).with_scope_slugs([WILDCARD_SCOPE]);
        assert!(admin.has_scope(scope_slug::WRITE_USERS));
        assert!(admin.has_scope(scope_slug::READ_REQUESTS));
    }

    /// `require_scope` denies a missing slug (Go's `privacy.Denyf("default
    /// deny")` fallback in `scopes.QueryPolicy::EvalQuery`).
    #[test]
    fn require_scope_denies_missing_slug() {
        let context = PolicyContext::new(
            Principal::user("7", ProjectScope::project_ids(["p1"]))
                .with_scope_slugs([scope_slug::READ_CHANNELS]),
        );

        assert_eq!(
            require_scope(&context, scope_slug::READ_CHANNELS),
            PolicyDecision::Allow
        );
        assert_eq!(
            require_scope(&context, scope_slug::WRITE_CHANNELS),
            PolicyDecision::Deny {
                reason: format!("required scope `{}` is missing", scope_slug::WRITE_CHANNELS),
            }
        );
    }

    /// System / Test keep their bypass so the pre-auth internal call paths
    /// (candidate selection, boot, schedulers) are unaffected.
    #[test]
    fn system_and_test_principals_still_bypass_scope_checks() {
        let system = PolicyContext::new(Principal::system());
        let test = PolicyContext::new(Principal::test());

        assert_eq!(
            require_scope(&system, scope_slug::WRITE_SETTINGS),
            PolicyDecision::Allow
        );
        assert_eq!(
            require_scope(&test, scope_slug::WRITE_SETTINGS),
            PolicyDecision::Allow
        );
    }

    /// The entity table must reproduce the Go schema mapping, including the
    /// entities Go gates on *another* entity's scope: `model.go` and
    /// `channel_model_price.go` use channel scopes, and `thread`/`trace`/
    /// `usage_log` use request scopes.
    #[test]
    fn entity_scope_mapping_matches_go_schemas() {
        assert_eq!(
            PolicyEntity::Channel.required_scope(ProjectAccess::Read),
            scope_slug::READ_CHANNELS
        );
        // model.go: `UserReadScopeRule(scopes.ScopeReadChannels)`.
        assert_eq!(
            PolicyEntity::Model.required_scope(ProjectAccess::Write),
            scope_slug::WRITE_CHANNELS
        );
        assert_eq!(
            PolicyEntity::ChannelModelPrice.required_scope(ProjectAccess::Read),
            scope_slug::READ_CHANNELS
        );
        // thread/trace/usage_log ride on the request scopes.
        assert_eq!(
            PolicyEntity::Thread.required_scope(ProjectAccess::Read),
            scope_slug::READ_REQUESTS
        );
        assert_eq!(
            PolicyEntity::UsageLog.required_scope(ProjectAccess::Write),
            scope_slug::WRITE_REQUESTS
        );
        // oidc_identity.go gates on user scopes.
        assert_eq!(
            PolicyEntity::OidcIdentity.required_scope(ProjectAccess::Write),
            scope_slug::WRITE_USERS
        );
        // api_key_profile_template.go gates on api-key scopes.
        assert_eq!(
            PolicyEntity::ApiKeyProfileTemplate.required_scope(ProjectAccess::Read),
            scope_slug::READ_API_KEYS
        );
    }

    /// `require_entity_access` composes both halves: the scope rule AND the
    /// project isolation. A member with the right scope but the wrong project is
    /// still denied.
    #[test]
    fn entity_access_enforces_scope_then_project_isolation() {
        let context = PolicyContext::new(
            Principal::user("7", ProjectScope::project_ids(["p1"]))
                .with_scope_slugs([scope_slug::READ_REQUESTS]),
        );

        // Right scope, own project -> allowed.
        assert_eq!(
            require_entity_access(
                &context,
                PolicyEntity::Request,
                ProjectAccess::Read,
                Some("p1")
            ),
            PolicyDecision::Allow
        );
        // Right scope, someone else's project -> denied by isolation.
        assert!(matches!(
            require_entity_access(
                &context,
                PolicyEntity::Request,
                ProjectAccess::Read,
                Some("p2")
            ),
            PolicyDecision::Deny { .. }
        ));
        // Own project but missing the write scope -> denied by the scope rule.
        assert!(matches!(
            require_entity_access(
                &context,
                PolicyEntity::Request,
                ProjectAccess::Write,
                Some("p1")
            ),
            PolicyDecision::Deny { .. }
        ));
    }

    /// Read and write are distinct slugs, so a read-only principal cannot
    /// mutate. Before this layer existed `require_project_access` ignored its
    /// `access` argument entirely, making the two indistinguishable.
    #[test]
    fn read_only_principal_cannot_write() {
        let context =
            PolicyContext::new(Principal::user("7", ProjectScope::All).with_scope_slugs([
                scope_slug::READ_CHANNELS,
                scope_slug::READ_USERS,
                scope_slug::READ_SETTINGS,
            ]));

        for entity in [
            PolicyEntity::Channel,
            PolicyEntity::User,
            PolicyEntity::System,
        ] {
            assert_eq!(
                require_entity_access(&context, entity, ProjectAccess::Read, None),
                PolicyDecision::Allow,
                "read must be allowed for {entity:?}"
            );
            assert!(
                matches!(
                    require_entity_access(&context, entity, ProjectAccess::Write, None),
                    PolicyDecision::Deny { .. }
                ),
                "write must be denied for {entity:?}"
            );
        }
    }
}

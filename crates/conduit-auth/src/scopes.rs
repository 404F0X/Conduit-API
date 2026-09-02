use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

pub mod slug {
    pub const SYSTEM_ADMIN: &str = "system:admin";

    /// Grant-all scope.
    ///
    /// Go has no wildcard rule — owner authority flows through `user.IsOwner`
    /// (`scopes.OwnerRule`, `internal/scopes/rule_owner.go`). But the Rust
    /// `initialize` seeds the owner user AND the built-in `Admin` project role
    /// with `scopes = ["*"]` (`system_service.rs` `default_project_roles`), so
    /// the scope matcher has to honour it or every Admin-role (non-owner) user
    /// would be denied outright.
    pub const WILDCARD: &str = "*";

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

/// Direct role/user scope slugs. Derived encodings such as `system:role:*` and
/// `project:*` are intentionally excluded.
pub const KNOWN_ROLE_SCOPE_SLUGS: &[&str] = &[
    slug::SYSTEM_ADMIN,
    slug::READ_DASHBOARD,
    slug::READ_CHANNELS,
    slug::WRITE_CHANNELS,
    slug::READ_DATA_STORAGES,
    slug::WRITE_DATA_STORAGES,
    slug::READ_USERS,
    slug::WRITE_USERS,
    slug::READ_SETTINGS,
    slug::WRITE_SETTINGS,
    slug::READ_ROLES,
    slug::WRITE_ROLES,
    slug::READ_PROJECTS,
    slug::WRITE_PROJECTS,
    slug::READ_API_KEYS,
    slug::WRITE_API_KEYS,
    slug::READ_REQUESTS,
    slug::WRITE_REQUESTS,
    slug::READ_PROMPTS,
    slug::WRITE_PROMPTS,
    slug::READ_GROUPS,
    slug::WRITE_GROUPS,
    slug::READ_SUBSCRIPTIONS,
    slug::WRITE_SUBSCRIPTIONS,
    slug::READ_BILLING,
    slug::WRITE_BILLING,
    slug::GRANT_CREDIT,
    slug::READ_COMMERCIALIZATION,
    slug::WRITE_COMMERCIALIZATION,
];

/// Whether `scope` is a recognized direct role/user scope slug.
pub fn is_known_scope_slug(scope: &str) -> bool {
    KNOWN_ROLE_SCOPE_SLUGS.contains(&scope)
}

/// Project roles may only carry scopes whose catalog level includes
/// `project`. Commercialization, billing, Group and other global operator
/// capabilities remain system-only even if a client crafts the mutation by
/// hand instead of using the filtered UI catalog.
pub fn supports_project_role(scope: &str) -> bool {
    matches!(
        scope,
        slug::READ_USERS
            | slug::WRITE_USERS
            | slug::READ_ROLES
            | slug::WRITE_ROLES
            | slug::READ_API_KEYS
            | slug::WRITE_API_KEYS
            | slug::READ_REQUESTS
            | slug::WRITE_REQUESTS
            | slug::READ_PROMPTS
            | slug::WRITE_PROMPTS
    )
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Scope(String);

impl Scope {
    pub fn new(scope: impl Into<String>) -> Self {
        Self(scope.into())
    }

    pub fn project(project_id: impl AsRef<str>, scope: impl AsRef<str>) -> Self {
        Self(format!(
            "project:{}:{}",
            project_id.as_ref(),
            scope.as_ref()
        ))
    }

    // Role-derived scopes keep their source encoded so RBAC can audit the match.
    pub fn system_role(scope: impl AsRef<str>) -> Self {
        Self(format!("system:role:{}", scope.as_ref()))
    }

    pub fn project_membership(project_id: impl AsRef<str>, scope: impl AsRef<str>) -> Self {
        Self(format!(
            "project:{}:member:{}",
            project_id.as_ref(),
            scope.as_ref()
        ))
    }

    pub fn project_role(project_id: impl AsRef<str>, scope: impl AsRef<str>) -> Self {
        Self(format!(
            "project:{}:role:{}",
            project_id.as_ref(),
            scope.as_ref()
        ))
    }

    // API key scopes are distinct from user scopes and are checked after user/RBAC grants.
    pub fn api_key(scope: impl AsRef<str>) -> Self {
        Self(format!("api_key:scope:{}", scope.as_ref()))
    }

    pub fn api_key_project(project_id: impl AsRef<str>, scope: impl AsRef<str>) -> Self {
        Self(format!(
            "api_key:project:{}:{}",
            project_id.as_ref(),
            scope.as_ref()
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Scope {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Scope {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<Scope> for String {
    fn from(value: Scope) -> Self {
        value.0
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Scope").field(&self.0).finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSet {
    scopes: BTreeSet<Scope>,
}

impl ScopeSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_strings<I, S>(scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            scopes: scopes
                .into_iter()
                .map(|scope| Scope::new(scope.into()))
                .collect(),
        }
    }

    pub fn insert(&mut self, scope: impl Into<Scope>) {
        self.scopes.insert(scope.into());
    }

    /// Look up `scope` in the set.
    ///
    /// The [`slug::WILDCARD`] entry matches every scope. Go has no such rule,
    /// but the Rust `initialize` seeds `scopes = ["*"]` on the owner user and the
    /// built-in `Admin` role, so honouring it here is what keeps those seeded
    /// principals authorized (see that constant's docs). Every derived check
    /// (`contains`, `matched_project_scope`, `matched_api_key_project_scope`, …)
    /// funnels through here, so the wildcard applies uniformly.
    pub fn matched(&self, scope: impl AsRef<str>) -> Option<&Scope> {
        if let Some(wildcard) = self.scopes.get(&Scope::new(slug::WILDCARD)) {
            return Some(wildcard);
        }
        self.scopes.get(&Scope::new(scope.as_ref()))
    }

    pub fn contains(&self, scope: impl AsRef<str>) -> bool {
        self.matched(scope).is_some()
    }

    pub fn contains_project_scope(
        &self,
        project_id: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> bool {
        self.matched_project_scope(project_id, scope).is_some()
    }

    pub fn matched_project_scope(
        &self,
        project_id: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> Option<&Scope> {
        let project_scope = Scope::project(project_id, scope.as_ref());
        self.scopes
            .get(&project_scope)
            .or_else(|| self.matched(scope.as_ref()))
    }

    pub fn matched_system_role_scope(&self, scope: impl AsRef<str>) -> Option<&Scope> {
        self.matched(slug::SYSTEM_ADMIN).or_else(|| {
            let role_scope = Scope::system_role(scope);
            self.scopes.get(&role_scope)
        })
    }

    pub fn matched_project_membership_scope(
        &self,
        project_id: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> Option<&Scope> {
        let scope = scope.as_ref();
        // `*` is also queried as the project-ownership marker. It may prove
        // ownership, but callers still cannot use it to satisfy an unsupported
        // concrete system scope because those checks return above.
        if scope != slug::WILDCARD && !supports_project_role(scope) {
            return None;
        }
        let project_id = project_id.as_ref();
        let membership_scope = Scope::project_membership(project_id, scope);
        self.scopes.get(&membership_scope).or_else(|| {
            self.scopes
                .get(&Scope::project_membership(project_id, slug::WILDCARD))
        })
    }

    pub fn matched_project_role_scope(
        &self,
        project_id: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> Option<&Scope> {
        let scope = scope.as_ref();
        if scope != slug::WILDCARD && !supports_project_role(scope) {
            return None;
        }
        let project_id = project_id.as_ref();
        let role_scope = Scope::project_role(project_id, scope);
        self.scopes.get(&role_scope).or_else(|| {
            self.scopes
                .get(&Scope::project_role(project_id, slug::WILDCARD))
        })
    }

    pub fn matched_api_key_scope(&self, scope: impl AsRef<str>) -> Option<&Scope> {
        let api_key_scope = Scope::api_key(scope);
        self.scopes.get(&api_key_scope)
    }

    pub fn matched_api_key_project_scope(
        &self,
        project_id: impl AsRef<str>,
        scope: impl AsRef<str>,
    ) -> Option<&Scope> {
        let api_key_project_scope = Scope::api_key_project(project_id, scope.as_ref());
        self.scopes
            .get(&api_key_project_scope)
            .or_else(|| self.matched_api_key_scope(scope))
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }
}

impl From<&BTreeSet<String>> for ScopeSet {
    fn from(scopes: &BTreeSet<String>) -> Self {
        Self::from_strings(scopes.iter().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_set_supports_string_and_project_scope() {
        let scopes = ScopeSet::from_strings([
            slug::READ_CHANNELS.to_string(),
            Scope::project("p1", slug::WRITE_CHANNELS).to_string(),
        ]);

        assert!(scopes.contains(slug::READ_CHANNELS));
        assert!(scopes.contains_project_scope("p1", slug::WRITE_CHANNELS));
        assert!(!scopes.contains_project_scope("p2", slug::WRITE_CHANNELS));
    }

    #[test]
    fn scope_set_supports_rbac_source_scopes() {
        let scopes = ScopeSet::from_strings([
            slug::SYSTEM_ADMIN.to_string(),
            Scope::project_membership("p1", slug::READ_USERS).to_string(),
            Scope::project_role("p1", slug::WRITE_USERS).to_string(),
            Scope::api_key_project("p1", slug::READ_REQUESTS).to_string(),
        ]);

        assert!(scopes.matched_system_role_scope(slug::READ_USERS).is_some());
        assert!(
            scopes
                .matched_project_membership_scope("p1", slug::READ_USERS)
                .is_some()
        );
        assert!(
            scopes
                .matched_project_role_scope("p1", slug::WRITE_USERS)
                .is_some()
        );
        assert!(
            scopes
                .matched_api_key_project_scope("p1", slug::READ_REQUESTS)
                .is_some()
        );
        assert!(
            scopes
                .matched_api_key_project_scope("p2", slug::READ_REQUESTS)
                .is_none()
        );
    }

    #[test]
    fn project_membership_and_role_wildcards_stay_project_scoped() {
        let scopes = ScopeSet::from_strings([
            Scope::project_membership("p1", slug::WILDCARD).to_string(),
            Scope::project_role("p2", slug::WILDCARD).to_string(),
        ]);
        assert!(
            scopes
                .matched_project_membership_scope("p1", slug::WRITE_USERS)
                .is_some()
        );
        assert!(
            scopes
                .matched_project_membership_scope("p2", slug::WRITE_USERS)
                .is_none()
        );
        assert!(
            scopes
                .matched_project_role_scope("p2", slug::WRITE_USERS)
                .is_some()
        );
        assert!(
            scopes
                .matched_project_role_scope("p1", slug::WRITE_USERS)
                .is_none()
        );
        assert!(!scopes.contains(slug::WRITE_USERS));
    }

    #[test]
    fn project_membership_and_role_cannot_grant_system_only_scopes() {
        let scopes = ScopeSet::from_strings([
            Scope::project_membership("p1", slug::WILDCARD).to_string(),
            Scope::project_membership("p1", slug::GRANT_CREDIT).to_string(),
            Scope::project_role("p1", slug::WILDCARD).to_string(),
            Scope::project_role("p1", slug::WRITE_SETTINGS).to_string(),
        ]);

        assert!(
            scopes
                .matched_project_membership_scope("p1", slug::WRITE_API_KEYS)
                .is_some()
        );
        assert!(
            scopes
                .matched_project_role_scope("p1", slug::READ_REQUESTS)
                .is_some()
        );
        assert!(
            scopes
                .matched_project_membership_scope("p1", slug::GRANT_CREDIT)
                .is_none()
        );
        assert!(
            scopes
                .matched_project_role_scope("p1", slug::WRITE_SETTINGS)
                .is_none()
        );
    }

    // ---- Go `internal/scopes/scopes_test.go` parity (adapted) -------------
    // Go tests `AllScopes`/`IsValidScope`/`ScopeSlug` constants. Rust
    // re-architected scopes as `ScopeSet` + `slug::*` constants (no global
    // `AllScopes`/`IsValidScope`), so these pin the *contract* the Go tests
    // guarded: every slug is non-empty, distinct, the Go-expected scope names
    // are present, and `Scope` round-trips its string. A future accidental
    // slug rename would break these.

    /// Mirrors Go `TestScopeConstants` (scopes_test.go:111-135).
    #[test]
    fn slug_constants_are_non_empty() {
        for s in [
            slug::SYSTEM_ADMIN,
            slug::READ_DASHBOARD,
            slug::READ_CHANNELS,
            slug::WRITE_CHANNELS,
            slug::READ_DATA_STORAGES,
            slug::WRITE_DATA_STORAGES,
            slug::READ_USERS,
            slug::WRITE_USERS,
            slug::READ_SETTINGS,
            slug::WRITE_SETTINGS,
            slug::READ_ROLES,
            slug::WRITE_ROLES,
            slug::READ_PROJECTS,
            slug::WRITE_PROJECTS,
            slug::READ_API_KEYS,
            slug::WRITE_API_KEYS,
            slug::READ_REQUESTS,
            slug::WRITE_REQUESTS,
            slug::READ_PROMPTS,
            slug::WRITE_PROMPTS,
            slug::READ_GROUPS,
            slug::WRITE_GROUPS,
            slug::READ_SUBSCRIPTIONS,
            slug::WRITE_SUBSCRIPTIONS,
            slug::READ_BILLING,
            slug::WRITE_BILLING,
            slug::GRANT_CREDIT,
            slug::READ_COMMERCIALIZATION,
            slug::WRITE_COMMERCIALIZATION,
        ] {
            assert!(!s.is_empty(), "slug constant must not be empty");
        }
    }

    /// Mirrors Go `TestAllScopes` (scopes_test.go:7-45): the Go-expected scope
    /// set is present among the Rust slug constants.
    #[test]
    fn slug_constants_cover_go_expected_scope_set() {
        let expected = [
            slug::READ_CHANNELS,
            slug::WRITE_CHANNELS,
            slug::READ_USERS,
            slug::WRITE_USERS,
            slug::READ_ROLES,
            slug::WRITE_ROLES,
            slug::READ_API_KEYS,
            slug::WRITE_API_KEYS,
            slug::READ_REQUESTS,
            slug::WRITE_REQUESTS,
            slug::READ_DASHBOARD,
            slug::READ_SETTINGS,
            slug::WRITE_SETTINGS,
        ];
        let all = [
            slug::SYSTEM_ADMIN,
            slug::READ_DASHBOARD,
            slug::READ_CHANNELS,
            slug::WRITE_CHANNELS,
            slug::READ_DATA_STORAGES,
            slug::WRITE_DATA_STORAGES,
            slug::READ_USERS,
            slug::WRITE_USERS,
            slug::READ_SETTINGS,
            slug::WRITE_SETTINGS,
            slug::READ_ROLES,
            slug::WRITE_ROLES,
            slug::READ_PROJECTS,
            slug::WRITE_PROJECTS,
            slug::READ_API_KEYS,
            slug::WRITE_API_KEYS,
            slug::READ_REQUESTS,
            slug::WRITE_REQUESTS,
            slug::READ_PROMPTS,
            slug::WRITE_PROMPTS,
            slug::READ_GROUPS,
            slug::WRITE_GROUPS,
            slug::READ_SUBSCRIPTIONS,
            slug::WRITE_SUBSCRIPTIONS,
            slug::READ_BILLING,
            slug::WRITE_BILLING,
            slug::GRANT_CREDIT,
            slug::READ_COMMERCIALIZATION,
            slug::WRITE_COMMERCIALIZATION,
        ];
        let all_set: BTreeSet<&str> = all.iter().copied().collect();
        for s in expected {
            assert!(
                all_set.contains(s),
                "expected Go scope {s:?} missing from slug constants"
            );
        }
    }

    /// Guards against accidental slug duplication across the module.
    #[test]
    fn slug_constants_are_distinct() {
        let all = [
            slug::SYSTEM_ADMIN,
            slug::READ_DASHBOARD,
            slug::READ_CHANNELS,
            slug::WRITE_CHANNELS,
            slug::READ_DATA_STORAGES,
            slug::WRITE_DATA_STORAGES,
            slug::READ_USERS,
            slug::WRITE_USERS,
            slug::READ_SETTINGS,
            slug::WRITE_SETTINGS,
            slug::READ_ROLES,
            slug::WRITE_ROLES,
            slug::READ_PROJECTS,
            slug::WRITE_PROJECTS,
            slug::READ_API_KEYS,
            slug::WRITE_API_KEYS,
            slug::READ_REQUESTS,
            slug::WRITE_REQUESTS,
            slug::READ_PROMPTS,
            slug::WRITE_PROMPTS,
            slug::READ_GROUPS,
            slug::WRITE_GROUPS,
            slug::READ_SUBSCRIPTIONS,
            slug::WRITE_SUBSCRIPTIONS,
            slug::READ_BILLING,
            slug::WRITE_BILLING,
            slug::GRANT_CREDIT,
            slug::READ_COMMERCIALIZATION,
            slug::WRITE_COMMERCIALIZATION,
        ];
        let mut seen = BTreeSet::new();
        for s in all {
            assert!(seen.insert(s), "duplicate slug constant: {s:?}");
        }
    }

    #[test]
    fn project_role_support_matches_scope_level_contract() {
        for scope in [
            slug::READ_USERS,
            slug::WRITE_USERS,
            slug::READ_ROLES,
            slug::WRITE_ROLES,
            slug::READ_API_KEYS,
            slug::WRITE_API_KEYS,
            slug::READ_REQUESTS,
            slug::WRITE_REQUESTS,
            slug::READ_PROMPTS,
            slug::WRITE_PROMPTS,
        ] {
            assert!(
                supports_project_role(scope),
                "{scope} must support project roles"
            );
        }
        for scope in [
            slug::READ_GROUPS,
            slug::READ_SUBSCRIPTIONS,
            slug::READ_BILLING,
            slug::GRANT_CREDIT,
            slug::READ_COMMERCIALIZATION,
            slug::SYSTEM_ADMIN,
        ] {
            assert!(is_known_scope_slug(scope));
            assert!(
                !supports_project_role(scope),
                "{scope} must remain system-role only"
            );
        }
    }

    /// Mirrors Go `TestScopeType` (scopes_test.go:136-143): `Scope` round-trips
    /// its slug string.
    #[test]
    fn scope_round_trips_slug_string() {
        let scope = Scope::new("test_scope");
        assert_eq!(scope.as_str(), "test_scope");
    }

    // ====================================================================
    // Go `internal/scopes/rule_test.go` parity — TestHasScope (L54-95).
    //
    // Go: `hasScope(scopes []string, required string) bool` =
    // `slices.Contains(scopes, required)`.
    // Rust analogue: `ScopeSet::contains(scope)`.
    // Existing tests cover the positive case; these fill the negative /
    // empty-set gaps.
    // ====================================================================

    /// Mirrors Go TestHasScope "scope does not exist" (rule_test.go:67-72):
    /// scopes=["read_users","write_users"], required="read_channels" -> false.
    #[test]
    fn go_has_scope_missing_scope_returns_false() {
        let scopes = ScopeSet::from_strings(["read_users", "write_users"]);
        assert!(!scopes.contains("read_channels"));
    }

    /// Mirrors Go TestHasScope "empty scopes" + "nil scopes" (rule_test.go:73-84):
    /// empty (or nil) scope slice -> false for any required scope.
    #[test]
    fn go_has_scope_empty_set_returns_false() {
        let empty = ScopeSet::new();
        assert!(!empty.contains("read_users"));
        assert!(!empty.contains("read_channels"));
    }

    // ====================================================================
    // Go `internal/scopes/rule_test.go` parity — TestHasRoleScope (L97-188).
    //
    // Go: `hasSystemRoleScope(user, required)` iterates `user.Edges.Roles`,
    // checking each system role's `Scopes` for `required`. Rust flattens
    // role-derived scopes into a single `ScopeSet` using the
    // `system:role:<scope>` prefix; `matched_system_role_scope` resolves
    // them (plus the `system:admin` wildcard).
    // Existing `scope_set_supports_rbac_source_scopes` tests the SYSTEM_ADMIN
    // wildcard path; these fill the specific system-role-scope gaps.
    // ====================================================================

    /// Mirrors Go TestHasRoleScope "user has role with required scope"
    /// (rule_test.go:104-119): role scopes=["read_users","write_users"],
    /// required=ScopeReadUsers -> true.
    #[test]
    fn go_has_system_role_scope_match_returns_some() {
        let scopes = ScopeSet::from_strings([
            Scope::system_role(slug::READ_USERS).to_string(),
            Scope::system_role(slug::WRITE_USERS).to_string(),
        ]);
        let matched = scopes.matched_system_role_scope(slug::READ_USERS);
        assert!(matched.is_some());
        assert_eq!(matched.map(|s| s.as_str()), Some("system:role:read_users"));
    }

    /// Mirrors Go TestHasRoleScope "user has role without required scope"
    /// (rule_test.go:120-135): role scopes=["read_channels","write_channels"],
    /// required=ScopeReadUsers -> false.
    #[test]
    fn go_has_system_role_scope_no_match_returns_none() {
        let scopes = ScopeSet::from_strings([
            Scope::system_role(slug::READ_CHANNELS).to_string(),
            Scope::system_role(slug::WRITE_CHANNELS).to_string(),
        ]);
        assert!(scopes.matched_system_role_scope(slug::READ_USERS).is_none());
    }

    /// Mirrors Go TestHasRoleScope "user has multiple roles, one with required
    /// scope" (rule_test.go:136-155): two roles, second has "read_users" -> true.
    /// In Rust, multiple `system:role:*` scopes coexist in one ScopeSet.
    #[test]
    fn go_has_system_role_scope_multiple_roles_one_matches() {
        let scopes = ScopeSet::from_strings([
            Scope::system_role(slug::READ_CHANNELS).to_string(),
            Scope::system_role(slug::READ_USERS).to_string(),
            Scope::system_role(slug::WRITE_USERS).to_string(),
        ]);
        // The matching scope is found among multiple system-role scopes.
        assert!(scopes.matched_system_role_scope(slug::READ_USERS).is_some());
        // The other scopes also resolve independently.
        assert!(
            scopes
                .matched_system_role_scope(slug::READ_CHANNELS)
                .is_some()
        );
        // A scope present in neither role is still absent.
        assert!(scopes.matched_system_role_scope(slug::READ_ROLES).is_none());
    }

    /// Mirrors Go TestHasRoleScope "user has no roles" + "empty roles"
    /// (rule_test.go:156-177): nil or empty roles slice -> false.
    #[test]
    fn go_has_system_role_scope_empty_set_returns_none() {
        let empty = ScopeSet::new();
        assert!(empty.matched_system_role_scope(slug::READ_USERS).is_none());
    }

    // ---- wildcard scope (Rust `initialize` seed parity) ------------------
    //
    // Go has no wildcard rule; owner authority flows through `user.IsOwner`.
    // But the Rust `initialize` seeds BOTH the owner user and the built-in
    // `Admin` project role with `scopes = ["*"]`
    // (`system_service.rs::default_project_roles`). Before this was honoured,
    // an Admin-role (non-owner) user held only the literal scope `"*"` and so
    // was denied every real slug — locking them out of the admin panel.

    #[test]
    fn wildcard_scope_matches_any_slug() {
        let scopes = ScopeSet::from_strings([slug::WILDCARD.to_string()]);
        assert!(scopes.contains(slug::READ_CHANNELS));
        assert!(scopes.contains(slug::WRITE_SETTINGS));
        assert!(scopes.contains(slug::READ_DASHBOARD));
    }

    #[test]
    fn wildcard_scope_satisfies_user_project_and_role_lookups() {
        // The wildcard is seeded onto *users* and the built-in `Admin` role, so
        // the user-facing matchers must honour it: `matched_project_scope` falls
        // back to the bare slug, and `matched_system_role_scope` funnels through
        // `matched`.
        let scopes = ScopeSet::from_strings([slug::WILDCARD.to_string()]);
        assert!(scopes.contains_project_scope("project-1", slug::WRITE_CHANNELS));
        assert!(scopes.matched_system_role_scope(slug::READ_USERS).is_some());
    }

    #[test]
    fn wildcard_does_not_leak_into_api_key_scopes() {
        // API keys are never seeded with `"*"` (`default_user_scopes` grants
        // explicit slugs only), and their scopes are authoritative under the
        // `api_key:` prefix. A stray wildcard must NOT silently promote an API
        // key to full authority — that would widen the OpenAPI surface beyond
        // what Go's `APIKeyScopeQueryRule` permits.
        let scopes = ScopeSet::from_strings([slug::WILDCARD.to_string()]);
        assert!(scopes.matched_api_key_scope(slug::READ_REQUESTS).is_none());
        assert!(
            scopes
                .matched_api_key_project_scope("project-1", slug::READ_REQUESTS)
                .is_none()
        );
    }

    #[test]
    fn without_wildcard_unrelated_slug_is_still_denied() {
        // The wildcard must not make the matcher permissive in general.
        let scopes = ScopeSet::from_strings([slug::READ_CHANNELS.to_string()]);
        assert!(scopes.contains(slug::READ_CHANNELS));
        assert!(!scopes.contains(slug::WRITE_CHANNELS));
        assert!(!scopes.contains(slug::READ_SETTINGS));
    }

    #[test]
    fn empty_scope_set_denies_everything() {
        let scopes = ScopeSet::new();
        assert!(!scopes.contains(slug::READ_CHANNELS));
        assert!(!scopes.contains(slug::WILDCARD));
    }
}

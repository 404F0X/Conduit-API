use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    System,
    User,
    ApiKey,
    Test,
}

/// Sub-classification of an API-key principal.
///
/// Mirrors the Go `apikey.Type` enum (`internal/ent/apikey/apikey.go`):
/// `user`, `service_account`, `noauth`. RBAC uses this to enforce the
/// OpenAPI/admin surface separation — service-account keys are restricted to
/// the OpenAPI GraphQL surface and must not reach the admin GraphQL surface,
/// regardless of which ordinary scopes they carry. See
/// `rbac::can_access_admin_graphql`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyKind {
    /// Ordinary per-user API key. Default for backward compatibility with
    /// callers that construct `Principal::api_key` without specifying a kind.
    #[default]
    User,
    /// Service-account API key — the only type permitted on the OpenAPI
    /// GraphQL surface (Go `WithOpenAPIAuth`, `auth.go:140-143`).
    ServiceAccount,
    /// System-managed noauth fallback key (Go `EnsureNoAuthAPIKey`).
    NoAuth,
}

impl ApiKeyKind {
    pub const fn is_service_account(self) -> bool {
        matches!(self, Self::ServiceAccount)
    }
}

impl fmt::Display for ApiKeyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::User => "user",
            Self::ServiceAccount => "service_account",
            Self::NoAuth => "noauth",
        })
    }
}

impl fmt::Display for PrincipalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::System => "system",
            Self::User => "user",
            Self::ApiKey => "api_key",
            Self::Test => "test",
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: Option<String>,
    pub project_id: Option<String>,
    pub scopes: BTreeSet<String>,
    pub is_owner: bool,
    pub session_scope: Option<String>,
    pub is_noauth: bool,
    /// API-key sub-classification. Only meaningful when `kind == ApiKey`;
    /// ignored for other principal kinds. Defaults to `ApiKeyKind::User` so
    /// existing constructors and serde payloads keep working.
    #[serde(default)]
    pub api_key_kind: ApiKeyKind,
}

impl Principal {
    pub fn system() -> Self {
        Self {
            kind: PrincipalKind::System,
            id: None,
            project_id: None,
            scopes: BTreeSet::new(),
            is_owner: true,
            session_scope: Some("system".to_string()),
            is_noauth: false,
            api_key_kind: ApiKeyKind::User,
        }
    }

    pub fn user(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            kind: PrincipalKind::User,
            id: Some(id.clone()),
            project_id: None,
            scopes: BTreeSet::new(),
            is_owner: false,
            session_scope: Some(format!("user:{id}")),
            is_noauth: false,
            api_key_kind: ApiKeyKind::User,
        }
    }

    pub fn api_key(id: impl Into<String>, project_id: impl Into<String>) -> Self {
        let id = id.into();
        let project_id = project_id.into();
        Self {
            kind: PrincipalKind::ApiKey,
            id: Some(id.clone()),
            project_id: Some(project_id.clone()),
            scopes: BTreeSet::new(),
            is_owner: false,
            session_scope: Some(format!("api_key:{id}:project:{project_id}")),
            is_noauth: false,
            api_key_kind: ApiKeyKind::User,
        }
    }

    /// Build a service-account API-key principal.
    ///
    /// Mirrors Go `WithOpenAPIAuth` (`internal/server/middleware/auth.go:140`),
    /// which injects a `PrincipalTypeAPIKey` principal only when the resolved
    /// `apikey.Type == TypeServiceAccount`. Such principals are barred from
    /// the admin GraphQL surface by `rbac::can_access_admin_graphql`.
    pub fn api_key_service_account(id: impl Into<String>, project_id: impl Into<String>) -> Self {
        let id = id.into();
        let project_id = project_id.into();
        Self {
            kind: PrincipalKind::ApiKey,
            id: Some(id.clone()),
            project_id: Some(project_id.clone()),
            scopes: BTreeSet::new(),
            is_owner: false,
            session_scope: Some(format!("api_key:{id}:project:{project_id}")),
            is_noauth: false,
            api_key_kind: ApiKeyKind::ServiceAccount,
        }
    }

    pub fn noauth_api_key(project_id: impl Into<String>) -> Self {
        let project_id = project_id.into();
        Self {
            kind: PrincipalKind::ApiKey,
            id: Some("noauth".to_string()),
            project_id: Some(project_id.clone()),
            scopes: BTreeSet::new(),
            is_owner: false,
            session_scope: Some(format!("api_key:noauth:project:{project_id}")),
            is_noauth: true,
            api_key_kind: ApiKeyKind::NoAuth,
        }
    }

    pub fn test() -> Self {
        Self {
            kind: PrincipalKind::Test,
            id: None,
            project_id: None,
            scopes: BTreeSet::new(),
            is_owner: true,
            session_scope: Some("test".to_string()),
            is_noauth: false,
            api_key_kind: ApiKeyKind::User,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.insert(scope.into());
        self
    }

    pub fn with_owner(mut self, is_owner: bool) -> Self {
        self.is_owner = is_owner;
        self
    }

    /// Override the API-key sub-classification. Intended for API-key
    /// principals; ignored for non-API-key kinds by `rbac`.
    pub fn with_api_key_kind(mut self, kind: ApiKeyKind) -> Self {
        self.api_key_kind = kind;
        self
    }

    pub fn safe_subject(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            PrincipalKind::System => f.write_str("system"),
            PrincipalKind::User => write!(f, "user:{}", self.id.as_deref().unwrap_or("unknown")),
            PrincipalKind::ApiKey => {
                write!(f, "apikey:{}", self.id.as_deref().unwrap_or("unknown"))
            }
            PrincipalKind::Test => f.write_str("test"),
        }
    }
}

impl fmt::Debug for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Principal")
            .field("kind", &self.kind)
            .field("subject", &self.to_string())
            .field("project_id", &self.project_id)
            .field("scopes", &self.scopes)
            .field("is_owner", &self.is_owner)
            .field("session_scope", &self.session_scope)
            .field("is_noauth", &self.is_noauth)
            .field("api_key_kind", &self.api_key_kind)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_is_compatible() {
        assert_eq!(Principal::system().to_string(), "system");
        assert_eq!(Principal::user("42").to_string(), "user:42");
        assert_eq!(
            Principal::api_key("key-1", "project-1").to_string(),
            "apikey:key-1"
        );
        assert_eq!(Principal::test().to_string(), "test");
    }

    #[test]
    fn debug_uses_safe_subject_not_secret() {
        let principal = Principal::api_key("key-id", "project-1").with_scope("models:read");
        let rendered = format!("{principal:?}");

        assert!(rendered.contains("apikey:key-id"));
        assert!(!rendered.contains("secret"));
    }

    // ====================================================================
    // Go `internal/authz/principal_test.go` parity (L1-310).
    //
    // Go: PrincipalType (iota int) + Principal{Type, UserID *int, APIKeyID
    // *int, ProjectID *int}. Rust: PrincipalKind (enum) + Principal{kind,
    // id Option<String>, project_id Option<String>, scopes, ...}. The Rust
    // redesign merges UserID/APIKeyID into a single `id: Option<String>`
    // and uses string ids throughout (DB-migration divergence). Tests below
    // mirror the Go golden cases adapted to the Rust API surface.
    // ====================================================================

    /// Mirrors Go `TestPrincipalType_String` (principal_test.go:10-28).
    ///
    /// Go `PrincipalType.String()` returns "system"/"user"/"apikey"/"unknown"
    /// (default branch for out-of-range values). Rust `PrincipalKind::Display`
    /// returns "system"/"user"/"api_key"/"test".
    ///
    /// PARITY DIVERGENCE (flagged, not a bug): Go uses "apikey" (no
    /// underscore); Rust `PrincipalKind::Display` uses "api_key" (snake_case,
    /// matching the serde rename). `Principal::Display` correctly uses the
    /// "apikey:" prefix matching Go's audit format (principal.go:84-89). The
    /// PrincipalKind form is internal-only (debug output, serde) and never
    /// appears in audit logs. Go's "unknown" case (`PrincipalType(999)`,
    /// principal_test.go:19) has no Rust equivalent — PrincipalKind is a
    /// closed enum with no unknown variant (structural divergence).
    #[test]
    fn go_principal_kind_display_forms_are_stable() {
        assert_eq!(PrincipalKind::System.to_string(), "system");
        assert_eq!(PrincipalKind::User.to_string(), "user");
        // DIVERGENCE: Go "apikey" vs Rust "api_key" — see doc comment.
        assert_eq!(PrincipalKind::ApiKey.to_string(), "api_key");
        assert_eq!(PrincipalKind::Test.to_string(), "test");
    }

    /// Mirrors Go `TestPrincipal_String` "user without id" + "apikey without
    /// id" cases (principal_test.go:98-101): a Principal with nil UserID /
    /// APIKeyID renders as "user:unknown" / "apikey:unknown".
    ///
    /// Existing `display_is_compatible` covers the with-id cases; this fills
    /// the two without-id gaps.
    #[test]
    fn go_principal_display_without_id_shows_unknown() {
        let mut user = Principal::user("temp");
        user.id = None;
        assert_eq!(user.to_string(), "user:unknown");

        let mut api_key = Principal::api_key("temp", "project-1");
        api_key.id = None;
        assert_eq!(api_key.to_string(), "apikey:unknown");
    }

    /// Mirrors Go `TestPrincipal_IsSystem` (principal_test.go:30-47),
    /// `TestPrincipal_IsUser` (L49-66), `TestPrincipal_IsAPIKey` (L68-85).
    ///
    /// Go: methods `IsSystem()`/`IsUser()`/`IsAPIKey()` on Principal.
    /// Rust: kind matching via `matches!`. This test pins the classification
    /// so a refactor cannot silently break it.
    #[test]
    fn go_principal_kind_classification_matches_go_is_methods() {
        // IsSystem (L30-47): system=true, user=false, apikey=false.
        assert!(matches!(Principal::system().kind, PrincipalKind::System));
        assert!(!matches!(Principal::user("1").kind, PrincipalKind::System));
        assert!(!matches!(
            Principal::api_key("k", "p").kind,
            PrincipalKind::System
        ));

        // IsUser (L49-66): user=true, system=false, apikey=false.
        assert!(matches!(Principal::user("1").kind, PrincipalKind::User));
        assert!(!matches!(Principal::system().kind, PrincipalKind::User));
        assert!(!matches!(
            Principal::api_key("k", "p").kind,
            PrincipalKind::User
        ));

        // IsAPIKey (L68-85): apikey=true, system=false, user=false.
        assert!(matches!(
            Principal::api_key("k", "p").kind,
            PrincipalKind::ApiKey
        ));
        assert!(!matches!(Principal::system().kind, PrincipalKind::ApiKey));
        assert!(!matches!(Principal::user("1").kind, PrincipalKind::ApiKey));
    }

    /// Mirrors Go `TestPrincipalEqual` (principal_test.go:259-310, 6 cases).
    ///
    /// Go: `principalEqual` compares Type/UserID/APIKeyID/ProjectID (via
    /// `intPtrEqual` on each `*int` field, principal.go:115-133). Rust:
    /// `PartialEq` derive compares all fields. These cases pin the
    /// Go-equivalent equality semantics on the identifying fields.
    #[test]
    fn go_principal_equality_cases() {
        // Case 1: same system -> true (L266-270).
        assert_eq!(Principal::system(), Principal::system());

        // Case 2: same user -> true (L273-277).
        assert_eq!(Principal::user("123"), Principal::user("123"));

        // Case 3: different user id -> false (L279-283).
        assert_ne!(Principal::user("123"), Principal::user("456"));

        // Case 4: different types -> false (L285-289).
        assert_ne!(
            Principal::user("123"),
            Principal::api_key("123", "project-1")
        );

        // Case 5: same apikey (same id + project_id) -> true (L291-295).
        assert_eq!(
            Principal::api_key("123", "456"),
            Principal::api_key("123", "456")
        );

        // Case 6: different project id -> false (L297-301).
        assert_ne!(
            Principal::api_key("123", "456"),
            Principal::api_key("123", "789")
        );
    }

    /// Mirrors Go `TestNewSystemContext` (principal_test.go:208-219),
    /// `TestNewUserContext` (L221-236), `TestNewAPIKeyContext` (L238-257).
    ///
    /// Go: `NewSystemContext`/`NewUserContext`/`NewAPIKeyContext` attach a
    /// Principal to `context.Context`. Rust: `Principal::system()` /
    /// `Principal::user(id)` / `Principal::api_key(id, project_id)`
    /// constructors. Verifies kind and identifying fields are set correctly.
    #[test]
    fn go_principal_constructors_match_new_context_helpers() {
        // NewSystemContext (L208-219): principal is system type.
        let system = Principal::system();
        assert!(matches!(system.kind, PrincipalKind::System));
        assert!(system.id.is_none());

        // NewUserContext (L221-236): principal is user type with id.
        let user = Principal::user("123");
        assert!(matches!(user.kind, PrincipalKind::User));
        assert_eq!(user.id.as_deref(), Some("123"));

        // NewAPIKeyContext (L238-257): principal is apikey with id + project.
        let api_key = Principal::api_key("456", "789");
        assert!(matches!(api_key.kind, PrincipalKind::ApiKey));
        assert_eq!(api_key.id.as_deref(), Some("456"));
        assert_eq!(api_key.project_id.as_deref(), Some("789"));
    }
}

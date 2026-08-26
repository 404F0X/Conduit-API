use std::sync::Arc;

use async_trait::async_trait;
use conduit_auth::{
    ApiKeyError, Claims, JwtError, NO_AUTH_SENTINEL, PasswordError, Scope, decode_hs256,
    reject_no_auth_sentinel, verify_password_bcrypt_hex,
};
use conduit_db::{RepoError, RequestContext};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type AuthServiceResult<T> = Result<T, AuthServiceError>;

#[derive(Debug, Error)]
pub enum AuthServiceError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    #[error(transparent)]
    ApiKey(#[from] ApiKeyError),
    #[error("jwt authentication failed: {0}")]
    Jwt(String),
    #[error("password verification failed: {0}")]
    Password(String),
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("user is not active: {0}")]
    UserInactive(String),
    #[error("password login is disabled for oidc-only user: {0}")]
    OidcOnlyUser(String),
    #[error("api key is not active: {0}")]
    ApiKeyInactive(String),
    #[error("api key project is not active: {0}")]
    ProjectInactive(String),
    #[error("openapi authentication requires a service account api key")]
    OpenApiRequiresServiceAccount,
    #[error("noauth api key is only available when api auth is disabled")]
    NoAuthApiKeyRejected,
    #[error("api auth is disabled but the noauth api key could not be resolved: {0}")]
    NoAuthApiKeyMissing(String),
    #[error("noauth authentication is not allowed")]
    NoAuthNotAllowed,
    /// Idempotency guard for system initialization — mirrors Go's
    /// `IsInitialized` early-return (`system.go` lines 658-667). Go treats a
    /// re-initialize as a silent no-op; the Rust port surfaces it so callers
    /// can detect duplicate/concurrent bootstrap attempts.
    #[error("system is already initialized")]
    SystemAlreadyInitialized,
}

impl From<JwtError> for AuthServiceError {
    fn from(error: JwtError) -> Self {
        Self::Jwt(error.to_string())
    }
}

impl From<PasswordError> for AuthServiceError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedUser {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub is_owner: bool,
    pub scope_slugs: Vec<String>,
    pub project_ids: Vec<String>,
    pub session_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedApiKey {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub key_type: AuthApiKeyType,
    pub scope_slugs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub password_bcrypt_hex: Option<String>,
    pub status: AuthUserStatus,
    pub is_owner: bool,
    pub scope_slugs: Vec<String>,
    pub project_ids: Vec<String>,
    pub oidc_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthUserStatus {
    Active,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthApiKey {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub status: AuthApiKeyStatus,
    pub project_status: AuthProjectStatus,
    pub key_type: AuthApiKeyType,
    pub scope_slugs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthApiKeyStatus {
    Active,
    Disabled,
    /// Mirrors Go `apikey.StatusArchived`; treated like `Disabled` by
    /// `ensure_active_api_key` (auth-side rejection of inactive keys).
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProjectStatus {
    Active,
    Archived,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthApiKeyType {
    ServiceAccount,
    User,
    /// System-managed api key that bypasses auth when `allow_no_auth` is enabled.
    /// Mirrors Go's `apikey.TypeNoauth`. The Go enum tag is the single word
    /// `"noauth"` (no underscore); override the snake_case default which would
    /// otherwise render `"no_auth"`.
    #[serde(rename = "noauth")]
    NoAuth,
}

#[async_trait]
pub trait AuthUserRepo: Send + Sync {
    async fn find_user_by_email(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> AuthServiceResult<Option<AuthUser>>;

    async fn find_user_by_id(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> AuthServiceResult<Option<AuthUser>>;
}

#[async_trait]
pub trait AuthApiKeyRepo: Send + Sync {
    async fn find_api_key_by_plaintext(
        &self,
        ctx: &RequestContext,
        plaintext_key: &str,
    ) -> AuthServiceResult<Option<AuthApiKey>>;

    /// Resolve (or lazily create) the system-managed noauth api key.
    /// Mirrors Go `APIKeyService.EnsureNoAuthAPIKey`. Only invoked when the
    /// service was constructed with `allow_no_auth = true`.
    async fn ensure_no_auth_api_key(&self, ctx: &RequestContext) -> AuthServiceResult<AuthApiKey>;
}

pub struct AuthService {
    user_repo: Arc<dyn AuthUserRepo>,
    api_key_repo: Arc<dyn AuthApiKeyRepo>,
    jwt_secret: Vec<u8>,
    /// When true, requests without credentials fall back to a system-managed
    /// noauth api key (Go `AllowNoAuth`).
    allow_no_auth: bool,
}

impl AuthService {
    pub fn new(
        user_repo: Arc<dyn AuthUserRepo>,
        api_key_repo: Arc<dyn AuthApiKeyRepo>,
        jwt_secret: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            user_repo,
            api_key_repo,
            jwt_secret: jwt_secret.into(),
            // Default to secure: callers opt in to noauth explicitly.
            allow_no_auth: false,
        }
    }

    /// Enable the noauth fallback path (Go `AllowNoAuth = true`).
    pub fn with_allow_no_auth(mut self, allow: bool) -> Self {
        self.allow_no_auth = allow;
        self
    }

    pub async fn authenticate_user(
        &self,
        ctx: &RequestContext,
        email: &str,
        password: &str,
    ) -> AuthServiceResult<AuthenticatedUser> {
        let user = self
            .user_repo
            .find_user_by_email(ctx, email)
            .await?
            .ok_or(AuthServiceError::InvalidCredentials)?;

        ensure_active_user(&user)?;
        let Some(password_bcrypt_hex) = user.password_bcrypt_hex.as_deref() else {
            return Err(AuthServiceError::OidcOnlyUser(user.id));
        };
        if user.oidc_only {
            return Err(AuthServiceError::OidcOnlyUser(user.id));
        }
        if !verify_password_bcrypt_hex(password, password_bcrypt_hex)? {
            return Err(AuthServiceError::InvalidCredentials);
        }

        Ok(user.into_authenticated(None))
    }

    pub async fn authenticate_jwt(
        &self,
        ctx: &RequestContext,
        token: &str,
    ) -> AuthServiceResult<AuthenticatedUser> {
        let claims = decode_hs256(token, &self.jwt_secret)?;
        // `claims.user_id` is `i64` (Go emits a numeric JWT `user_id`); the user
        // repo's lookup is keyed by the string form of the id, mirroring Go's
        // `strconv.Itoa(user.ID)` usage in the session-scope path.
        let user_id_str = claims.user_id.to_string();
        let user = self
            .user_repo
            .find_user_by_id(ctx, &user_id_str)
            .await?
            .ok_or(AuthServiceError::InvalidCredentials)?;

        ensure_active_user(&user)?;
        Ok(user.into_authenticated(Some(claims)))
    }

    pub async fn authenticate_api_key(
        &self,
        ctx: &RequestContext,
        api_key: &str,
    ) -> AuthServiceResult<AuthenticatedApiKey> {
        reject_no_auth_sentinel(api_key)?;
        let api_key = self
            .api_key_repo
            .find_api_key_by_plaintext(ctx, api_key)
            .await?
            .ok_or(AuthServiceError::InvalidCredentials)?;

        ensure_active_api_key(&api_key)?;
        // Noauth keys are reserved for the `allow_no_auth` fallback and may not
        // be presented directly by clients (Go auth.go:227-229).
        if api_key.key_type == AuthApiKeyType::NoAuth {
            return Err(AuthServiceError::NoAuthApiKeyRejected);
        }
        Ok(api_key.into_authenticated())
    }

    pub async fn authenticate_openapi(
        &self,
        ctx: &RequestContext,
        api_key: &str,
    ) -> AuthServiceResult<AuthenticatedApiKey> {
        let authenticated = self.authenticate_api_key(ctx, api_key).await?;
        if authenticated.key_type != AuthApiKeyType::ServiceAccount {
            return Err(AuthServiceError::OpenApiRequiresServiceAccount);
        }

        Ok(authenticated)
    }

    /// Authenticate a request that arrived without credentials by falling back
    /// to the system-managed noauth api key. Only succeeds when this service
    /// was constructed with `allow_no_auth = true` (Go auth.go:234-260).
    pub async fn authenticate_no_auth(
        &self,
        ctx: &RequestContext,
    ) -> AuthServiceResult<AuthenticatedApiKey> {
        if !self.allow_no_auth {
            return Err(AuthServiceError::NoAuthNotAllowed);
        }

        let api_key = self.api_key_repo.ensure_no_auth_api_key(ctx).await?;
        ensure_active_api_key(&api_key)?;
        // Defensive: the noauth key must carry the NoAuth type, otherwise the
        // repo implementation has drifted from the contract.
        if api_key.key_type != AuthApiKeyType::NoAuth {
            return Err(AuthServiceError::NoAuthApiKeyMissing(api_key.id));
        }
        Ok(api_key.into_authenticated())
    }
}

impl AuthUser {
    fn into_authenticated(self, claims: Option<Claims>) -> AuthenticatedUser {
        AuthenticatedUser {
            id: self.id,
            email: self.email,
            display_name: self.display_name,
            is_owner: self.is_owner,
            scope_slugs: self.scope_slugs,
            project_ids: self.project_ids,
            session_scope: claims.map(|claims| claims.session_scope),
        }
    }
}

impl AuthApiKey {
    fn into_authenticated(self) -> AuthenticatedApiKey {
        AuthenticatedApiKey {
            id: self.id,
            project_id: self.project_id,
            name: self.name,
            key_type: self.key_type,
            // Store API-key scopes in the same shape consumed by conduit-auth RBAC.
            scope_slugs: self
                .scope_slugs
                .into_iter()
                .map(|scope| Scope::api_key(scope).to_string())
                .collect(),
        }
    }
}

fn ensure_active_user(user: &AuthUser) -> AuthServiceResult<()> {
    if user.status == AuthUserStatus::Active {
        Ok(())
    } else {
        Err(AuthServiceError::UserInactive(user.id.clone()))
    }
}

fn ensure_active_api_key(api_key: &AuthApiKey) -> AuthServiceResult<()> {
    if api_key.status != AuthApiKeyStatus::Active {
        return Err(AuthServiceError::ApiKeyInactive(api_key.id.clone()));
    }
    if api_key.project_status != AuthProjectStatus::Active {
        return Err(AuthServiceError::ProjectInactive(
            api_key.project_id.clone(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// S10 — pure no-auth decision logic (mirrors Go auth.go AuthenticateNoAuth
// plus the middleware's "key present?" precondition).
// ---------------------------------------------------------------------------

/// Outcome of evaluating the no-auth fallback path for a single request.
///
/// Mirrors the Go middleware's branch logic (`middleware/auth.go`) combined
/// with `AuthService.AuthenticateNoAuth` (`auth.go` lines 234-260):
/// - When `allow_no_auth` is true and no API key was supplied, the request
///   resolves to the system-managed noauth principal.
/// - When the caller literally presents `CONDUIT_API_KEY_NO_AUTH`, the request
///   is rejected regardless of the `allow_no_auth` flag (Go `AuthenticateAPIKey`
///   would otherwise resolve the sentinel to a real key — the same rejection
///   is enforced by `reject_no_auth_sentinel` on the api-key path).
/// - When `allow_no_auth` is false and no key was supplied, the request must
///   be rejected with a "key required" error.
/// - When a real (non-sentinel) key was supplied, normal api-key auth applies
///   and the no-auth path is not engaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAuthDecision {
    /// No credentials were supplied and `allow_no_auth = true`: the request
    /// should be authenticated as the system-managed noauth principal.
    UseNoAuthPrincipal,
    /// A real (non-sentinel) API key was supplied: route through normal
    /// api-key authentication. The carried string is the trimmed key.
    AuthenticateKey(String),
    /// The literal sentinel `CONDUIT_API_KEY_NO_AUTH` was presented by the
    /// client. Always rejected — the sentinel is internal-only.
    RejectNoAuthSentinel,
    /// No credentials were supplied and `allow_no_auth = false`: the request
    /// is unauthorized.
    RejectNoAuthDisabled,
}

/// Pure policy mirror of Go's no-auth decision branch.
///
/// `allow_no_auth` is the server `api.auth.allow_no_auth` flag; `provided_key`
/// is the trimmed credential the caller presented (empty/whitespace = absent).
///
/// This function is deliberately pure (no async, no repo) so it can be
/// unit-tested without a database and reused by HTTP middleware that needs
/// to choose between `authenticate_api_key` and `authenticate_no_auth`.
pub fn decide_no_auth(allow_no_auth: bool, provided_key: &str) -> NoAuthDecision {
    let trimmed = provided_key.trim();
    if trimmed.is_empty() {
        if allow_no_auth {
            NoAuthDecision::UseNoAuthPrincipal
        } else {
            NoAuthDecision::RejectNoAuthDisabled
        }
    } else if trimmed == NO_AUTH_SENTINEL {
        // The sentinel is internal-only: presenting it literally is always
        // rejected (Go auth.go:227-229 + AuthenticateNoAuth guards).
        NoAuthDecision::RejectNoAuthSentinel
    } else {
        NoAuthDecision::AuthenticateKey(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// S17 — declarative system-initialize plan + pure precondition guard
// (mirrors Go system.go Initialize, lines 656-780).
// ---------------------------------------------------------------------------

/// Ordered, all-or-nothing bootstrap steps that `SystemService::initialize`
/// must run inside a single transaction.
///
/// This enum is intentionally declarative (no I/O): it captures the Go
/// `SystemService.Initialize` step ordering (system.go lines 656-780) so that
/// callers, tests, and a future `TxRepo` wiring can verify the plan without
/// touching a database. The async `SystemService::initialize` is the
/// executor; [`InitializePlan`] is its contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitializeStep {
    /// Create the owner user (email + hashed password, `is_owner=true`,
    /// scopes `["*"]`). Go system.go lines 699-710.
    CreateOwnerUser,
    /// Create the default project named `"Default"` and assign the owner.
    /// Go system.go lines 717-726.
    CreateDefaultProject,
    /// Persist the JWT secret key under `system_jwt_secret_key`.
    /// Go system.go lines 730-734.
    SetSecretKey,
    /// Persist the brand name. Go system.go lines 737-740.
    SetBrandName,
    /// Seed the default project roles (Admin/Developer/Viewer). Performed by
    /// Go `ProjectService.CreateProject` (project.go lines 86-140) which the
    /// Rust port invokes explicitly.
    SeedDefaultRoles,
    /// Persist primary data storage + default id. Go system.go lines 743-761.
    /// (Deferred until a `DataStorageRepo` lands — still part of the plan.)
    CreatePrimaryDataStorage,
    /// Record the build version (Go `build.Version`). Go system.go lines 769-773.
    SetVersion,
    /// Set the `system_initialized = "true"` flag **last**, so a crash mid-flow
    /// leaves the system un-initialized and re-runnable. Go system.go lines 763-767.
    MarkInitialized,
}

/// Declarative ordered plan for `SystemService::initialize`.
///
/// Built via [`InitializePlan::default`] (the canonical Go ordering) or
/// constructed explicitly in tests. The plan is consumed by the async
/// initializer; this type only describes *what* runs and in *what order*,
/// not how — keeping it pure and unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializePlan {
    steps: Vec<InitializeStep>,
}

impl InitializePlan {
    /// The canonical Go ordering (`system.go` `Initialize`, lines 656-780).
    ///
    /// `MarkInitialized` is deliberately last: it must only flip after every
    /// preceding step succeeded, so a mid-flow failure leaves the system
    /// re-initializable (Go commits the whole transaction at the end).
    pub fn canonical() -> Self {
        Self {
            steps: vec![
                InitializeStep::CreateOwnerUser,
                InitializeStep::CreateDefaultProject,
                InitializeStep::SetSecretKey,
                InitializeStep::SetBrandName,
                InitializeStep::SeedDefaultRoles,
                // CreatePrimaryDataStorage is part of the Go contract but the
                // Rust DataStorageRepo is not yet ported; it remains in the
                // canonical plan so the ordering is preserved byte-for-byte.
                InitializeStep::CreatePrimaryDataStorage,
                InitializeStep::SetVersion,
                InitializeStep::MarkInitialized,
            ],
        }
    }

    /// Steps in execution order.
    pub fn steps(&self) -> &[InitializeStep] {
        &self.steps
    }

    /// True when the plan enforces the Go invariant: `MarkInitialized` runs
    /// only after every other canonical step. The async initializer relies on
    /// this to guarantee "all or rollback" semantics inside its transaction.
    pub fn marks_initialized_last(&self) -> bool {
        self.steps.last() == Some(&InitializeStep::MarkInitialized)
    }
}

impl Default for InitializePlan {
    fn default() -> Self {
        Self::canonical()
    }
}

/// Pure precondition guard for `SystemService::initialize`.
///
/// Mirrors Go's idempotency check (`system.go` lines 658-667): if the system
/// is already initialized, the call is a no-op success in Go but the Rust
/// port surfaces [`AuthServiceError::SystemAlreadyInitialized`] so callers
/// can distinguish a genuine first-run from a duplicate/concurrent attempt.
///
/// `already_initialized` is the value of `system_initialized` as read by
/// `SystemService::is_initialized` immediately before the write transaction
/// opens. This guard is the concurrency/idempotency chokepoint: it must run
/// *before* any mutation, and the DB unique index on the owner email is the
/// tie-breaker for the lost race (see `system_service.rs` initialize docs).
pub fn validate_initialize_preconditions(already_initialized: bool) -> AuthServiceResult<()> {
    if already_initialized {
        return Err(AuthServiceError::SystemAlreadyInitialized);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Duration;
    use conduit_auth::{Claims, NO_AUTH_SENTINEL, encode_hs256, encode_password_bcrypt_hex, slug};
    use conduit_db::{PolicyContext, Principal};
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeAuthRepo {
        users_by_id: Mutex<BTreeMap<String, AuthUser>>,
        user_ids_by_email: Mutex<BTreeMap<String, String>>,
        api_keys_by_plaintext: Mutex<BTreeMap<String, AuthApiKey>>,
        no_auth_api_key: Mutex<Option<AuthApiKey>>,
    }

    impl FakeAuthRepo {
        async fn insert_user(&self, user: AuthUser) {
            self.user_ids_by_email
                .lock()
                .await
                .insert(user.email.clone(), user.id.clone());
            self.users_by_id.lock().await.insert(user.id.clone(), user);
        }

        async fn insert_api_key(&self, plaintext: &str, api_key: AuthApiKey) {
            self.api_keys_by_plaintext
                .lock()
                .await
                .insert(plaintext.to_string(), api_key);
        }

        async fn set_no_auth_api_key(&self, api_key: AuthApiKey) {
            *self.no_auth_api_key.lock().await = Some(api_key);
        }
    }

    #[async_trait]
    impl AuthUserRepo for FakeAuthRepo {
        async fn find_user_by_email(
            &self,
            _ctx: &RequestContext,
            email: &str,
        ) -> AuthServiceResult<Option<AuthUser>> {
            let Some(user_id) = self.user_ids_by_email.lock().await.get(email).cloned() else {
                return Ok(None);
            };

            Ok(self.users_by_id.lock().await.get(&user_id).cloned())
        }

        async fn find_user_by_id(
            &self,
            _ctx: &RequestContext,
            user_id: &str,
        ) -> AuthServiceResult<Option<AuthUser>> {
            Ok(self.users_by_id.lock().await.get(user_id).cloned())
        }
    }

    #[async_trait]
    impl AuthApiKeyRepo for FakeAuthRepo {
        async fn find_api_key_by_plaintext(
            &self,
            _ctx: &RequestContext,
            plaintext_key: &str,
        ) -> AuthServiceResult<Option<AuthApiKey>> {
            Ok(self
                .api_keys_by_plaintext
                .lock()
                .await
                .get(plaintext_key)
                .cloned())
        }

        async fn ensure_no_auth_api_key(
            &self,
            _ctx: &RequestContext,
        ) -> AuthServiceResult<AuthApiKey> {
            let mut guard = self.no_auth_api_key.lock().await;
            if let Some(existing) = guard.as_ref() {
                return Ok(existing.clone());
            }
            // Lazily create a noauth key mirroring Go EnsureNoAuthAPIKey.
            let created = AuthApiKey {
                id: "noauth-key".to_string(),
                project_id: "project-1".to_string(),
                name: "No Auth System Key".to_string(),
                status: AuthApiKeyStatus::Active,
                project_status: AuthProjectStatus::Active,
                key_type: AuthApiKeyType::NoAuth,
                scope_slugs: vec![],
            };
            *guard = Some(created.clone());
            Ok(created)
        }
    }

    fn test_ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn service(repo: Arc<FakeAuthRepo>) -> AuthService {
        AuthService::new(repo.clone(), repo, "jwt-secret")
    }

    fn active_user(password_bcrypt_hex: Option<String>) -> AuthUser {
        AuthUser {
            id: "1".to_string(),
            email: "user@example.com".to_string(),
            display_name: "User One".to_string(),
            password_bcrypt_hex,
            status: AuthUserStatus::Active,
            is_owner: false,
            scope_slugs: vec![slug::READ_CHANNELS.to_string()],
            project_ids: vec!["project-1".to_string()],
            oidc_only: false,
        }
    }

    fn api_key(key_type: AuthApiKeyType) -> AuthApiKey {
        AuthApiKey {
            id: "key-1".to_string(),
            project_id: "project-1".to_string(),
            name: "default".to_string(),
            status: AuthApiKeyStatus::Active,
            project_status: AuthProjectStatus::Active,
            key_type,
            scope_slugs: vec![slug::READ_REQUESTS.to_string()],
        }
    }

    fn noauth_api_key() -> AuthApiKey {
        AuthApiKey {
            id: "noauth-key".to_string(),
            project_id: "project-1".to_string(),
            name: "No Auth System Key".to_string(),
            status: AuthApiKeyStatus::Active,
            project_status: AuthProjectStatus::Active,
            key_type: AuthApiKeyType::NoAuth,
            scope_slugs: vec![],
        }
    }

    fn service_with_no_auth(repo: Arc<FakeAuthRepo>) -> AuthService {
        service(repo).with_allow_no_auth(true)
    }

    #[tokio::test]
    async fn authenticate_user_verifies_bcrypt_hex_password() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_user(active_user(Some(encode_password_bcrypt_hex("correct", 4)?)))
            .await;
        let service = service(repo);

        let authenticated = service
            .authenticate_user(&test_ctx(), "user@example.com", "correct")
            .await?;

        assert_eq!(authenticated.id, "1");
        assert_eq!(authenticated.email, "user@example.com");
        assert!(matches!(
            service
                .authenticate_user(&test_ctx(), "user@example.com", "wrong")
                .await,
            Err(AuthServiceError::InvalidCredentials)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn oidc_only_placeholder_user_cannot_password_login() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let mut user = active_user(None);
        user.oidc_only = true;
        repo.insert_user(user).await;
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_user(&test_ctx(), "user@example.com", "anything")
                .await,
            Err(AuthServiceError::OidcOnlyUser(user_id)) if user_id == "1"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_jwt_decodes_claims_and_loads_active_user() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_user(active_user(None)).await;
        let service = service(repo);
        let token = encode_hs256(&Claims::new(1, "session:project:project-1"), "jwt-secret")?;

        let authenticated = service.authenticate_jwt(&test_ctx(), &token).await?;

        assert_eq!(authenticated.id, "1");
        assert_eq!(
            authenticated.session_scope.as_deref(),
            Some("session:project:project-1")
        );
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_api_key_rejects_no_auth_sentinel_literal() {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_api_key(&test_ctx(), NO_AUTH_SENTINEL)
                .await,
            Err(AuthServiceError::ApiKey(ApiKeyError::NoAuthSentinel))
        ));
    }

    #[tokio::test]
    async fn authenticate_api_key_requires_enabled_key_and_active_project() -> AuthServiceResult<()>
    {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_api_key("ak-valid", api_key(AuthApiKeyType::User))
            .await;
        repo.insert_api_key(
            "ak-disabled",
            AuthApiKey {
                status: AuthApiKeyStatus::Disabled,
                ..api_key(AuthApiKeyType::User)
            },
        )
        .await;
        repo.insert_api_key(
            "ak-archived-project",
            AuthApiKey {
                project_status: AuthProjectStatus::Archived,
                ..api_key(AuthApiKeyType::User)
            },
        )
        .await;
        let service = service(repo);

        let authenticated = service
            .authenticate_api_key(&test_ctx(), "ak-valid")
            .await?;
        assert_eq!(
            authenticated.scope_slugs,
            vec![Scope::api_key(slug::READ_REQUESTS).to_string()]
        );
        assert!(matches!(
            service.authenticate_api_key(&test_ctx(), "ak-disabled").await,
            Err(AuthServiceError::ApiKeyInactive(key_id)) if key_id == "key-1"
        ));
        assert!(matches!(
            service
                .authenticate_api_key(&test_ctx(), "ak-archived-project")
                .await,
            Err(AuthServiceError::ProjectInactive(project_id)) if project_id == "project-1"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_openapi_only_allows_service_account_api_keys() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_api_key("ak-user", api_key(AuthApiKeyType::User))
            .await;
        repo.insert_api_key("ak-service", api_key(AuthApiKeyType::ServiceAccount))
            .await;
        let service = service(repo);

        assert!(matches!(
            service.authenticate_openapi(&test_ctx(), "ak-user").await,
            Err(AuthServiceError::OpenApiRequiresServiceAccount)
        ));
        let authenticated = service
            .authenticate_openapi(&test_ctx(), "ak-service")
            .await?;
        assert_eq!(authenticated.key_type, AuthApiKeyType::ServiceAccount);
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_jwt_rejects_expired_token() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_user(active_user(None)).await;
        let service = service(repo);

        // Claims with a TTL well past jsonwebtoken's default 60s leeway so the
        // exp validation actually rejects the token.
        let expired = encode_hs256(
            &Claims::with_ttl(1, "session:project:project-1", Duration::minutes(-5)),
            "jwt-secret",
        )?;

        assert!(matches!(
            service.authenticate_jwt(&test_ctx(), &expired).await,
            Err(AuthServiceError::Jwt(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_jwt_rejects_token_signed_with_wrong_secret() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_user(active_user(None)).await;
        let service = service(repo);

        let token = encode_hs256(
            &Claims::new(1, "session:project:project-1"),
            "different-secret",
        )?;

        assert!(matches!(
            service.authenticate_jwt(&test_ctx(), &token).await,
            Err(AuthServiceError::Jwt(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_jwt_rejects_token_for_missing_user() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service(repo);
        let token = encode_hs256(&Claims::new(999, "session:project:project-1"), "jwt-secret")?;

        assert!(matches!(
            service.authenticate_jwt(&test_ctx(), &token).await,
            Err(AuthServiceError::InvalidCredentials)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_jwt_rejects_inactive_user() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let mut user = active_user(None);
        user.status = AuthUserStatus::Disabled;
        repo.insert_user(user).await;
        let service = service(repo);
        let token = encode_hs256(&Claims::new(1, "session:project:project-1"), "jwt-secret")?;

        assert!(matches!(
            service.authenticate_jwt(&test_ctx(), &token).await,
            Err(AuthServiceError::UserInactive(user_id)) if user_id == "1"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_api_key_rejects_noauth_type_key_presented_directly()
    -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        // A noauth key leaked into the plaintext lookup path must be rejected
        // even if it is not the literal sentinel (Go auth.go:227-229).
        repo.insert_api_key("ak-noauth-leaked", noauth_api_key())
            .await;
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_api_key(&test_ctx(), "ak-noauth-leaked")
                .await,
            Err(AuthServiceError::NoAuthApiKeyRejected)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_no_auth_rejected_when_allow_no_auth_disabled() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service(repo); // allow_no_auth defaults to false

        assert!(matches!(
            service.authenticate_no_auth(&test_ctx()).await,
            Err(AuthServiceError::NoAuthNotAllowed)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_no_auth_lazily_creates_noauth_principal_when_allowed()
    -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service_with_no_auth(repo);

        let authenticated = service.authenticate_no_auth(&test_ctx()).await?;

        assert_eq!(authenticated.id, "noauth-key");
        assert_eq!(authenticated.key_type, AuthApiKeyType::NoAuth);
        // Re-invocation returns the same lazily-created key (idempotent).
        let again = service.authenticate_no_auth(&test_ctx()).await?;
        assert_eq!(again.id, "noauth-key");
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_no_auth_fails_when_repo_returns_non_noauth_key() -> AuthServiceResult<()>
    {
        let repo = Arc::new(FakeAuthRepo::default());
        // Repo drift: ensure_no_auth_api_key returns a regular user key.
        repo.set_no_auth_api_key(api_key(AuthApiKeyType::User))
            .await;
        let service = service_with_no_auth(repo);

        assert!(matches!(
            service.authenticate_no_auth(&test_ctx()).await,
            Err(AuthServiceError::NoAuthApiKeyMissing(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn authenticate_no_auth_fails_when_noauth_key_inactive() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.set_no_auth_api_key(AuthApiKey {
            status: AuthApiKeyStatus::Disabled,
            ..noauth_api_key()
        })
        .await;
        let service = service_with_no_auth(repo);

        assert!(matches!(
            service.authenticate_no_auth(&test_ctx()).await,
            Err(AuthServiceError::ApiKeyInactive(key_id)) if key_id == "noauth-key"
        ));
        Ok(())
    }

    // -------------------------------------------------------------------------
    // S10 — decide_no_auth pure policy (mirrors Go auth.go:234-260 + middleware).
    // -------------------------------------------------------------------------

    #[test]
    fn decide_no_auth_no_key_with_allow_true_uses_noauth_principal() {
        assert_eq!(decide_no_auth(true, ""), NoAuthDecision::UseNoAuthPrincipal);
    }

    #[test]
    fn decide_no_auth_whitespace_only_key_treated_as_absent() {
        // Tabs/spaces are trimmed before the absence check, matching the
        // middleware's "no credential supplied" branch.
        assert_eq!(
            decide_no_auth(true, "   \t  "),
            NoAuthDecision::UseNoAuthPrincipal
        );
    }

    #[test]
    fn decide_no_auth_no_key_with_allow_false_rejects_as_disabled() {
        assert_eq!(
            decide_no_auth(false, ""),
            NoAuthDecision::RejectNoAuthDisabled
        );
    }

    #[test]
    fn decide_no_auth_sentinel_literal_always_rejected_when_allowed() {
        // The sentinel is internal-only: even with allow_no_auth=true the
        // caller may not present it literally (Go auth.go:227-229).
        assert_eq!(
            decide_no_auth(true, NO_AUTH_SENTINEL),
            NoAuthDecision::RejectNoAuthSentinel
        );
    }

    #[test]
    fn decide_no_auth_sentinel_literal_always_rejected_when_disabled() {
        assert_eq!(
            decide_no_auth(false, NO_AUTH_SENTINEL),
            NoAuthDecision::RejectNoAuthSentinel
        );
    }

    #[test]
    fn decide_no_auth_real_key_routes_to_api_key_auth_even_when_allowed() {
        // A real key always wins over the noauth fallback — the fallback only
        // engages when *no* credential was supplied.
        let decision = decide_no_auth(true, "conduit-deadbeef");
        assert!(matches!(
            decision,
            NoAuthDecision::AuthenticateKey(ref k) if k == "conduit-deadbeef"
        ));
    }

    #[test]
    fn decide_no_auth_trims_real_key_before_routing() {
        let decision = decide_no_auth(true, "  conduit-deadbeef\n");
        assert!(matches!(
            decision,
            NoAuthDecision::AuthenticateKey(ref k) if k == "conduit-deadbeef"
        ));
    }

    // -------------------------------------------------------------------------
    // S17 — InitializePlan + validate_initialize_preconditions.
    // -------------------------------------------------------------------------

    #[test]
    fn initialize_plan_canonical_step_order_matches_go() {
        let plan = InitializePlan::canonical();
        // Go system.go Initialize (lines 656-780) executes in this exact
        // order; MarkInitialized is last so a crash leaves the system
        // re-runnable.
        let expected = [
            InitializeStep::CreateOwnerUser,
            InitializeStep::CreateDefaultProject,
            InitializeStep::SetSecretKey,
            InitializeStep::SetBrandName,
            InitializeStep::SeedDefaultRoles,
            InitializeStep::CreatePrimaryDataStorage,
            InitializeStep::SetVersion,
            InitializeStep::MarkInitialized,
        ];
        assert_eq!(plan.steps(), expected);
    }

    #[test]
    fn initialize_plan_default_equals_canonical() {
        assert_eq!(InitializePlan::default(), InitializePlan::canonical());
    }

    #[test]
    fn initialize_plan_marks_initialized_last_in_canonical_order() {
        // Go invariant: the initialized flag is the final write inside the
        // transaction (system.go lines 763-767), guaranteeing all-or-rollback.
        assert!(InitializePlan::canonical().marks_initialized_last());
    }

    #[test]
    fn initialize_plan_custom_order_can_break_marks_initialized_last() {
        // Sanity: a hand-built plan without MarkInitialized last is detected.
        let malformed = InitializePlan {
            steps: vec![
                InitializeStep::CreateOwnerUser,
                InitializeStep::MarkInitialized,
                InitializeStep::SetSecretKey,
            ],
        };
        assert!(!malformed.marks_initialized_last());
    }

    #[test]
    fn validate_initialize_preconditions_accepts_first_run() -> AuthServiceResult<()> {
        // Fresh system: is_initialized returns false -> proceed.
        validate_initialize_preconditions(false)?;
        Ok(())
    }

    #[test]
    fn validate_initialize_preconditions_rejects_already_initialized() {
        // Go treats re-initialize as a silent no-op; the Rust port surfaces
        // SystemAlreadyInitialized so duplicate/concurrent calls are visible.
        assert!(matches!(
            validate_initialize_preconditions(true),
            Err(AuthServiceError::SystemAlreadyInitialized)
        ));
    }

    // -------------------------------------------------------------------------
    // RUST-P15-001 S03 — auth_test.go pure-logic parity pins.
    //
    // The three standalone pure-function tests from auth_test.go —
    // TestHashPassword (L24), TestVerifyPassword (L38), TestGenerateSecretKey
    // (L58) — are already covered in conduit-auth (password.rs::hash_password_*,
    // verify_password_*, apikey.rs::generate_secret_key_*).
    //
    // The tests below cover service-level scenarios that the Go suite exercises
    // via an ent client but which are unit-testable with FakeAuthRepo.
    // -------------------------------------------------------------------------

    /// Go parity pin (`auth_test.go:227-230` `TestAuthService_AuthenticateUser`):
    /// authenticating with an email not present in the user repo must fail.
    /// Go's ent query returns NotFound → "invalid email or password"; Rust's
    /// `find_user_by_email` returns `None` → `InvalidCredentials`.
    #[tokio::test]
    async fn authenticate_user_rejects_unknown_email_with_invalid_credentials() {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_user(&test_ctx(), "nobody@example.com", "any")
                .await,
            Err(AuthServiceError::InvalidCredentials)
        ));
    }

    /// Go parity pin (`auth_test.go:232-238` `TestAuthService_AuthenticateUser`):
    /// a user whose status is not active must not be able to authenticate even
    /// with the correct password.
    ///
    /// PARITY NOTE: Go queries `WHERE status = activated` (auth.go:131), so a
    /// deactivated user appears as NotFound and returns "invalid email or
    /// password" (auth.go:137). Rust's `find_user_by_email` returns the user
    /// regardless of status, then `ensure_active_user` rejects with
    /// `UserInactive`. The observable outcome (authentication rejected) is
    /// identical; the error variant differs. Flagged for Leader.
    #[tokio::test]
    async fn authenticate_user_rejects_disabled_user() -> AuthServiceResult<()> {
        let repo = Arc::new(FakeAuthRepo::default());
        let mut user = active_user(Some(encode_password_bcrypt_hex("correct", 4)?));
        user.status = AuthUserStatus::Disabled;
        repo.insert_user(user).await;
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_user(&test_ctx(), "user@example.com", "correct")
                .await,
            Err(AuthServiceError::UserInactive(user_id)) if user_id == "1"
        ));
        Ok(())
    }

    /// Go parity pin (`auth_test.go:389-392` `TestAuthService_AuthenticateAPIKey`):
    /// an API key string not present in the repo must be rejected. Go wraps the
    /// lookup error with "failed to get api key" (auth.go:211); Rust returns
    /// `InvalidCredentials` (same rejection, different error text).
    #[tokio::test]
    async fn authenticate_api_key_rejects_unknown_key() {
        let repo = Arc::new(FakeAuthRepo::default());
        let service = service(repo);

        assert!(matches!(
            service
                .authenticate_api_key(&test_ctx(), "conduit-nonexistent")
                .await,
            Err(AuthServiceError::InvalidCredentials)
        ));
    }

    /// Go parity pin (`auth_test.go:289-292` `TestAuthService_AuthenticateJWTToken`):
    /// a completely malformed token string (not a valid JWT structure) must be
    /// rejected with a JWT error, mirroring Go's "failed to parse jwt token"
    /// (auth.go:179).
    #[tokio::test]
    async fn authenticate_jwt_rejects_malformed_token_string() {
        let repo = Arc::new(FakeAuthRepo::default());
        repo.insert_user(active_user(None)).await;
        let service = service(repo);

        assert!(matches!(
            service.authenticate_jwt(&test_ctx(), "not-a-jwt").await,
            Err(AuthServiceError::Jwt(_))
        ));
    }

    // -------------------------------------------------------------------------
    // Pending DB-backed subtests from auth_test.go (require ent client and/or
    // miniredis; their pure-logic assertions are covered by the tests above):
    //   - TestAuthService_GenerateJWTToken            (L130-180)  — needs ent client + SystemService for token generation + claim verification
    //   - TestAuthService_AuthenticateUser            (L182-239)  — needs ent client (logic covered: correct/wrong password, unknown email, disabled user)
    //   - TestAuthService_AuthenticateJWTToken        (L241-322)  — needs ent client + cache-hit assertion (logic covered: valid/expired/wrong-secret/malformed/inactive/missing-user)
    //   - TestAuthService_AuthenticateAPIKey          (L324-422)  — needs ent client (logic covered: valid/disabled/archived-project/unknown/noauth-type)
    //   - TestAuthService_AuthenticateNoAuth          (L424-474)  — needs ent client (logic covered: noauth-via-apikey-rejected, noauth-fallback-allowed)
    //   - TestAuthService_AuthenticateNoAuth_DisabledByConfig (L476-488) — needs ent client (logic covered: noauth-rejected-when-disabled)
    //   - TestAuthService_WithDifferentCacheConfigs   (L490-569)  — needs ent client + miniredis (cache mode matrix is infra, not biz logic)
    //   - TestAuthService_CacheExpiration             (L571-644)  — needs ent client + miniredis (TTL expiry is cache-infra, not biz logic)
    // -------------------------------------------------------------------------
}

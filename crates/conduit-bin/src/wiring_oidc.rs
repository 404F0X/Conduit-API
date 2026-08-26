//! ADPT-OIDC — host adapter implementing
//! [`conduit_http::oidc_handlers::OidcService`] (the currently-None `oidc`
//! AppServices slot). Fills user-facing single sign-on: `/oauth/oidc/*` +
//! `/admin/oidc/link/*`.
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//! - `biz.OIDCService` — `conduit/internal/server/biz/oidc.go`
//!   (`CountProviders` :338, `GetProviders` :407, `GetAuthorizeURL` :508,
//!   `GetLinkAuthorizeURL` :601, `Callback` :616, `ExchangeCode` :1252,
//!   `resolveUser` :931, `createIdentity` :1239, `fetchUserInfo` :790).
//! - `biz.AuthService` — `conduit/internal/server/biz/auth.go`
//!   (`GenerateJWTToken` :100, `AuthenticateJWTToken` :160).
//! - `api.OIDCHandlers` — `conduit/internal/server/api/oidc.go` (routes
//!   registered under the `/oauth` group, routes.go:91-94/120).
//!
//! ## Provider config storage (with Go refs)
//! Go loads `OIDCConfig.Providers []OIDCProvider` via fx from the app config
//! (oidc.go:160-162, 184-190) — **not** the database. The Rust host mirrors
//! this exactly: the provider list lives in `conduit_config::model::OidcConfig`
//! (`config.oidc.providers`), seeded from YAML. Each `OidcProviderConfig`
//! carries the subset Go's `OIDCProvider` needs to build an authorize URL:
//! `name` (id), `issuer_url`, `client_id`, `client_secret`, `scopes`.
//!
//! Fields Go has but the Rust config lacks (`display_name`, `icon_url`,
//! `button_color`, `enable_pkce`, manual `auth_url`/`token_url`, role-mapping
//! rules) are derived/ defaulted here and called out inline; they are not
//! fabricated.
//!
//! ## JWT secret
//! `authenticate_jwt_token` / `generate_jwt_token` sign+verify with the same
//! HS256 secret the API-auth path reads: `config.api_auth.jwt_secret`
//! (`Option<String>`). The admin JWT guard (`jwt_admin_auth`, routes.go:96)
//! and the `oidc_handlers` test mock (oidc_handlers.rs:862) use this same
//! source, so tokens round-trip consistently across the OIDC routes.
//!
//! ## Random-token encoding deviation (documented)
//! Go mints the CSRF `state` as `base64.RawURLEncoding.EncodeToString(32
//! random bytes)` (oidc.go:567-569, 43 URL-safe chars). The bin crate has no
//! `rand`/`base64` dependency, so this host reuses
//! `conduit_auth::generate_secret_key()` (32 CSPRNG bytes, hex-encoded → 64
//! chars, 256-bit entropy). It is an opaque one-shot CSRF token; only
//! URL-safety + entropy matter, both preserved (same approach as
//! `wiring_oauth_admin.rs`).
//!
//! Production callbacks perform discovery, code exchange, JWKS signature
//! verification, issuer/audience validation, optional userinfo lookup, and
//! PostgreSQL-backed identity linking/JIT provisioning. Tests replace only the
//! external IdP endpoints; the callback and database paths remain real.
//!
//! ## Leader wiring (do NOT self-wire)
//! 1. `crates/conduit-bin/src/main.rs`: `mod wiring_oidc;`
//! 2. In `wiring.rs::build_services` (its signature currently takes only
//!    `&DatabaseConfig`, so the Leader must also thread the OIDC + auth config
//!    slices in — e.g. widen the signature or construct the adapter at the
//!    call site that owns the full `AppConfig`):
//!    ```ignore
//!    let oidc: std::sync::Arc<dyn conduit_http::oidc_handlers::OidcService> =
//!        std::sync::Arc::new(crate::wiring_oidc::OidcAdapter::new(
//!            config.oidc.clone(),
//!            config.api_auth.jwt_secret.clone(),
//!        ));
//!    let services = services.with_oidc_service(oidc);
//!    ```
//!    (`AppServices::with_oidc_service` — app_state.rs:56.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use conduit_auth::generate_secret_key;
use conduit_auth::jwt::{Claims, decode_hs256, encode_hs256};
use conduit_config::model::{OidcConfig, OidcProviderConfig};
use conduit_http::oidc_handlers::{OidcExchangedUser, OidcService, ProviderInfo};
// `query_escape` is the verbatim Go `url.QueryEscape` port (oidc_handlers.rs:
// 212-224), re-exportd at the crate root (lib.rs:91). Used to build the
// authorize URL exactly like Go's `oauth2.Config.AuthCodeURL`.
use conduit_http::query_escape;

// ---------------------------------------------------------------------------
// Pure helpers — Go url.* parity
// ---------------------------------------------------------------------------

/// Go `url.Values.Encode()`: keys sorted alphabetically, both key and value
/// `url.QueryEscape`d, pairs joined with `&`. Go's `oauth2.Config.AuthCodeURL`
/// builds the authorize URL query via `v.Encode()` (oauth2.go AuthCodeURL), so
/// replicating the sort + escape is what makes the URL byte-faithful.
fn encode_query(pairs: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = pairs.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    sorted
        .iter()
        .map(|(key, value)| format!("{}={}", query_escape(key), query_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Go `normalizeOIDCProviderIdentifier` (oidc.go:141-143): lowercase + strip
/// spaces, used to match a route `{provider}` segment against config entries
/// regardless of casing/spacing.
fn normalize_identifier(value: &str) -> String {
    value.trim().to_lowercase().replace(' ', "")
}

/// Go `OIDCProvider.normalize()` (oidc.go:100-123) for the Rust config subset:
/// when the `name` is blank there is no id fallback here (the Rust config has
/// only `name`, no separate `id`/`display_name`), so the identifier is the
/// trimmed name verbatim.
fn provider_identifier(provider: &OidcProviderConfig) -> String {
    provider.name.trim().to_string()
}

/// Go `findOIDCProviderConfig` (oidc.go:342-351) + `matchesIdentifier`
/// (oidc.go:145-158): match the route identifier against the provider `name`
/// (the Rust config's only identifier field), case/space-insensitive.
fn find_provider<'a>(
    providers: &'a [OidcProviderConfig],
    identifier: &str,
) -> Option<&'a OidcProviderConfig> {
    let normalized = normalize_identifier(identifier);
    if normalized.is_empty() {
        return None;
    }
    providers
        .iter()
        .find(|provider| normalize_identifier(&provider.name) == normalized)
}

/// Go `OIDCProvider.issuer()` (oidc.go:133-139): the manual `Issuer` field
/// wins, else `IssuerURL`. The Rust config has only `issuer_url` (no separate
/// `issuer`), so this is `issuer_url` verbatim.
fn provider_issuer(provider: &OidcProviderConfig) -> String {
    provider.issuer_url.clone()
}

/// Go default scopes when `ExtraScopes` is empty (oidc.go:286-288, 533-535):
/// `["openid", "profile", "email"]`. The Rust config `OidcProviderConfig`
/// defaults to the same triple, but an explicit empty `scopes` vec still maps
/// to this default to match Go.
fn effective_scopes(provider: &OidcProviderConfig) -> Vec<String> {
    if provider.scopes.is_empty() {
        vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
        ]
    } else {
        provider.scopes.clone()
    }
}

/// Go redirect-path resolution (oidc.go:277-283, 527-530): `/oauth/oidc/callback`
/// for a single provider, `/oauth/oidc/callback/{id}` when more than one is
/// configured. The Rust config has no per-provider `redirect_url` override, so
/// the default path is always used.
fn default_redirect_path(provider_id: &str, provider_count: usize) -> String {
    if provider_count > 1 {
        format!("/oauth/oidc/callback/{provider_id}")
    } else {
        "/oauth/oidc/callback".to_string()
    }
}

/// Go `GetAuthorizeURL` redirect absolutization (oidc.go:562-565): if the
/// redirect path is relative and a `base_url` was supplied, prepend it.
fn absolutize_redirect(redirect_path: &str, base_url: &str) -> String {
    if !base_url.is_empty() && !redirect_path.starts_with("http") {
        format!("{base_url}{redirect_path}")
    } else {
        redirect_path.to_string()
    }
}

/// Build the authorize URL query + path exactly like Go's
/// `oauth2.Config.AuthCodeURL(state)` (oidc.go:596): the authorization endpoint
/// plus a `url.Values.Encode()`-sorted query of `client_id`, `redirect_uri`,
/// `response_type=code`, `scope` (space-joined), and `state`.
///
/// PKCE (`code_challenge` / `code_challenge_method=S256`, oidc.go:579-594) is
/// SKIPPED: the Rust `OidcProviderConfig` has no `enable_pkce` field, so Go's
/// `p.config.EnablePKCE` branch is never taken.
fn build_authorize_url(
    authorize_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    scopes: &[String],
    state: &str,
) -> String {
    let scope_value = scopes.join(" ");
    let pairs: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("scope", &scope_value),
        ("state", state),
    ];
    let separator = if authorize_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!("{authorize_endpoint}{separator}{}", encode_query(pairs))
}

// ---------------------------------------------------------------------------
// OIDC discovery seam — the live IdP round-trip for the authorize endpoint
// ---------------------------------------------------------------------------

/// Resolves the IdP `authorization_endpoint` for an issuer. Go does this at
/// startup via `oidc.NewProvider(ctx, issuerURL)` (oidc.go:233), which GETs
/// `{issuer}/.well-known/openid-configuration`; this host does it lazily on
/// the first `get_authorize_url` for each issuer and caches the result
/// (mirroring Go's in-memory `providers` map, oidc.go:295).
#[async_trait]
trait OidcDiscovery: Send + Sync {
    async fn fetch_authorize_endpoint(&self, issuer_url: &str) -> Result<String, String>;

    async fn fetch_document(&self, _issuer_url: &str) -> Result<OidcDiscoveryDocument, String> {
        Err("OIDC discovery metadata is unavailable".to_string())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct OidcDiscoveryDocument {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

/// Production discovery backed by `reqwest` — GETs the OIDC well-known
/// document and extracts `authorization_endpoint` (RFC 8414 / the coreos
/// go-oidc contract). The IdP may live behind a proxy / take time, so this is
/// the one live-network seam of the REAL methods.
struct ReqwestDiscovery {
    client: reqwest::Client,
}

impl ReqwestDiscovery {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl OidcDiscovery for ReqwestDiscovery {
    async fn fetch_authorize_endpoint(&self, issuer_url: &str) -> Result<String, String> {
        Ok(self
            .fetch_document(issuer_url)
            .await?
            .authorization_endpoint)
    }

    async fn fetch_document(&self, issuer_url: &str) -> Result<OidcDiscoveryDocument, String> {
        let trimmed = issuer_url.trim_end_matches('/');
        let well_known = format!("{trimmed}/.well-known/openid-configuration");
        let response = self
            .client
            .get(&well_known)
            .send()
            .await
            .map_err(|err| format!("OIDC discovery request failed for {trimmed}: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!(
                "OIDC discovery for {trimmed} returned status {status}"
            ));
        }
        let document: OidcDiscoveryDocument = response
            .json()
            .await
            .map_err(|err| format!("OIDC discovery document parse failed for {trimmed}: {err}"))?;
        Ok(document)
    }
}

// ---------------------------------------------------------------------------
// In-process state cache — stands in for the Go `xcache.Cache[[]byte]` seam
// ---------------------------------------------------------------------------

/// One cached state value with its expiry. Models the two Go cache families:
/// `oidc_state:{state}` → `"1"` (CSRF proof, oidc.go:572) and
/// `oidc_link_state:{state}` → user-id string (link intent, oidc.go:608).
struct CachedState {
    value: String,
    expires_at: DateTime<Utc>,
}

/// In-process TTL map mirroring Go `xcache` Set/Get/Delete semantics: an
/// expired entry behaves exactly like a missing one. The Go default xcache
/// backend is in-memory; a Redis port is a separate wiring gap.
#[derive(Default)]
struct StateCache {
    entries: Mutex<HashMap<String, CachedState>>,
}

/// Poisoned-mutex error string surfaced as the Go cache-failure 500 rather
/// than a panic (workspace forbids `unwrap`).
const CACHE_POISONED: &str = "oidc state cache poisoned";

impl StateCache {
    fn set(&self, key: String, value: String, ttl: Duration) -> Result<(), String> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| CACHE_POISONED.to_string())?;
        guard.insert(
            key,
            CachedState {
                value,
                expires_at: Utc::now()
                    + chrono::Duration::from_std(ttl).map_or(chrono::Duration::zero(), |d| d),
            },
        );
        Ok(())
    }

    /// Consuming lookup (Go `cache.Get` + `cache.Delete`, oidc.go:626/631):
    /// returns the value only if present and unexpired, then removes it.
    fn consume(&self, key: &str) -> Result<Option<String>, String> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| CACHE_POISONED.to_string())?;
        match guard.remove(key) {
            Some(entry) if entry.expires_at > Utc::now() => Ok(Some(entry.value)),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct PersistedOidcUser {
    id: i64,
    email: String,
    status: String,
    prefer_language: String,
    first_name: String,
    avatar: Option<String>,
    is_owner: bool,
    scopes: Value,
}

#[derive(Debug)]
struct PersistedLinkedIdentity {
    id: i64,
    issuer: String,
    email: Option<String>,
}

/// Backend-neutral persistence seam for the OIDC login/link lifecycle. OIDC
/// needs more than the read-only `OidcRepo` surface: it also provisions users,
/// links identities, updates last-login metadata, and loads the JWT secret.
#[async_trait]
trait OidcPersistence: Send + Sync {
    async fn load_system_value(&self, key: &str) -> Result<Option<String>, String>;
    async fn find_identity_user_id(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<i64>, String>;
    async fn touch_identity(
        &self,
        issuer: &str,
        subject: &str,
        email: &Option<String>,
        idp_name: &str,
    ) -> Result<(), String>;
    async fn find_user_id_by_email(&self, email: &str) -> Result<Option<i64>, String>;
    async fn create_oidc_user(
        &self,
        email: &str,
        password: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<i64, String>;
    async fn upsert_identity(
        &self,
        issuer: &str,
        subject: &str,
        email: &Option<String>,
        idp_name: &str,
        user_id: i64,
    ) -> Result<u64, String>;
    async fn load_user(&self, user_id: i64) -> Result<Option<PersistedOidcUser>, String>;
    async fn linked_identities(&self, user_id: i64)
    -> Result<Vec<PersistedLinkedIdentity>, String>;
}

struct PostgresOidcPersistence {
    pool: PgPool,
}

impl PostgresOidcPersistence {
    fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OidcPersistence for PostgresOidcPersistence {
    async fn load_system_value(&self, key: &str) -> Result<Option<String>, String> {
        sqlx::query_scalar("SELECT value FROM systems WHERE key = $1 AND deleted_at = 0")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| format!("failed to load system value: {error}"))
    }

    async fn find_identity_user_id(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<i64>, String> {
        sqlx::query_scalar(
            "SELECT user_id FROM oidc_identities \
             WHERE issuer = $1 AND subject = $2 AND deleted_at = 0",
        )
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("failed to query OIDC identity: {error}"))
    }

    async fn touch_identity(
        &self,
        issuer: &str,
        subject: &str,
        email: &Option<String>,
        idp_name: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "UPDATE oidc_identities SET email = $1, idp_name = $2, \
             last_login_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE issuer = $3 AND subject = $4 AND deleted_at = 0",
        )
        .bind(email)
        .bind(idp_name)
        .bind(issuer)
        .bind(subject)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to update OIDC identity: {error}"))
    }

    async fn find_user_id_by_email(&self, email: &str) -> Result<Option<i64>, String> {
        sqlx::query_scalar("SELECT id FROM users WHERE email = $1 AND deleted_at = 0")
            .bind(email)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| format!("failed to query OIDC user: {error}"))
    }

    async fn create_oidc_user(
        &self,
        email: &str,
        password: &str,
        first_name: &str,
        last_name: &str,
    ) -> Result<i64, String> {
        sqlx::query_scalar(
            "INSERT INTO users (email, status, prefer_language, password, first_name, \
             last_name, is_owner, scopes) \
             VALUES ($1, 'activated', 'en', $2, $3, $4, FALSE, '[]'::jsonb) RETURNING id",
        )
        .bind(email)
        .bind(password)
        .bind(first_name)
        .bind(last_name)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| format!("failed to create OIDC user: {error}"))
    }

    async fn upsert_identity(
        &self,
        issuer: &str,
        subject: &str,
        email: &Option<String>,
        idp_name: &str,
        user_id: i64,
    ) -> Result<u64, String> {
        sqlx::query(
            "INSERT INTO oidc_identities \
             (issuer, subject, email, idp_name, last_login_at, user_id) \
             VALUES ($1, $2, $3, $4, CURRENT_TIMESTAMP, $5) \
             ON CONFLICT(issuer, subject, deleted_at) DO UPDATE SET \
             email = excluded.email, idp_name = excluded.idp_name, \
             last_login_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE oidc_identities.user_id = excluded.user_id",
        )
        .bind(issuer)
        .bind(subject)
        .bind(email)
        .bind(idp_name)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(|error| format!("failed to save OIDC identity: {error}"))
    }

    async fn load_user(&self, user_id: i64) -> Result<Option<PersistedOidcUser>, String> {
        let row = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                String,
                Option<String>,
                bool,
                sqlx::types::Json<Value>,
            ),
        >(
            "SELECT id, email, status, prefer_language, first_name, avatar, is_owner, scopes \
             FROM users WHERE id = $1 AND deleted_at = 0",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("failed to load OIDC user: {error}"))?;
        Ok(row.map(|row| PersistedOidcUser {
            id: row.0,
            email: row.1,
            status: row.2,
            prefer_language: row.3,
            first_name: row.4,
            avatar: row.5,
            is_owner: row.6,
            scopes: row.7.0,
        }))
    }

    async fn linked_identities(
        &self,
        user_id: i64,
    ) -> Result<Vec<PersistedLinkedIdentity>, String> {
        sqlx::query_as::<_, (i64, String, Option<String>)>(
            "SELECT id, issuer, email FROM oidc_identities \
             WHERE user_id = $1 AND deleted_at = 0 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| PersistedLinkedIdentity {
                    id: row.0,
                    issuer: row.1,
                    email: row.2,
                })
                .collect()
        })
        .map_err(|error| format!("failed to list OIDC identities: {error}"))
    }
}

/// Host-side [`OidcService`] implementation: config-driven provider list,
/// HS256 JWT verify/sign with the api-auth secret, lazy OIDC discovery for the
/// authorize endpoint, and an in-process CSRF/link-state cache.
pub struct OidcAdapter {
    oidc: OidcConfig,
    jwt_secret: Option<String>,
    discovery: Box<dyn OidcDiscovery>,
    /// Cached `authorization_endpoint` per issuer (oidc.go:295 providers map).
    discovered_endpoints: Mutex<HashMap<String, String>>,
    state_cache: StateCache,
    persistence: Option<Arc<dyn OidcPersistence>>,
    http: reqwest::Client,
}

impl OidcAdapter {
    /// Production constructor. `oidc` is `config.oidc`; `jwt_secret` is
    /// `config.api_auth.jwt_secret` — the same secret the admin JWT guard
    /// reads, so bearer tokens round-trip across the OIDC routes.
    pub fn new_postgres(oidc: OidcConfig, jwt_secret: Option<String>, pool: PgPool) -> Self {
        let mut adapter = Self::with_discovery(oidc, jwt_secret, Box::new(ReqwestDiscovery::new()));
        adapter.persistence = Some(Arc::new(PostgresOidcPersistence::new(pool)));
        adapter
    }

    /// Constructor with an injectable discovery seam (tests). The production
    /// path uses [`ReqwestDiscovery`] (live well-known document fetch).
    fn with_discovery(
        oidc: OidcConfig,
        jwt_secret: Option<String>,
        discovery: Box<dyn OidcDiscovery>,
    ) -> Self {
        Self {
            oidc,
            jwt_secret,
            discovery,
            discovered_endpoints: Mutex::new(HashMap::new()),
            state_cache: StateCache::default(),
            persistence: None,
            http: reqwest::Client::new(),
        }
    }

    fn persistence(&self) -> Result<&Arc<dyn OidcPersistence>, String> {
        self.persistence
            .as_ref()
            .ok_or_else(|| "OIDC database is not configured".to_string())
    }

    /// Resolve the signing/verifying secret, erroring with the same shape Go
    /// uses when the system secret is unavailable (auth.go:103-106). The
    /// `jwt_admin_auth` middleware rejects tokens before the handler when the
    /// secret is absent, so reaching `authenticate_jwt_token` without one is a
    /// wiring gap surfaced as a plain error string.
    async fn secret(&self) -> Result<Vec<u8>, String> {
        if let Some(secret) = self
            .jwt_secret
            .as_deref()
            .filter(|secret| !secret.is_empty())
        {
            return Ok(secret.as_bytes().to_vec());
        }
        let value = self
            .persistence()
            .map_err(|_| "jwt secret is not configured".to_string())?
            .load_system_value(conduit_services::system_key::JWT_SECRET_KEY)
            .await?
            .ok_or_else(|| "jwt secret is not configured".to_string())?;
        decode_hex_secret(&value)
    }

    /// Lazily discover + cache the `authorization_endpoint` for an issuer
    /// (Go caches at startup in the `providers` map; this host caches on first
    /// use so a transient discovery failure can be retried on the next
    /// request, unlike Go's startup-only `log.Error ... continue`).
    async fn authorize_endpoint_for(&self, issuer_url: &str) -> Result<String, String> {
        if let Ok(guard) = self.discovered_endpoints.lock()
            && let Some(endpoint) = guard.get(issuer_url)
        {
            return Ok(endpoint.clone());
        }
        let endpoint = self.discovery.fetch_authorize_endpoint(issuer_url).await?;
        if let Ok(mut guard) = self.discovered_endpoints.lock() {
            guard.insert(issuer_url.to_string(), endpoint.clone());
        }
        Ok(endpoint)
    }

    async fn exchange_and_verify(
        &self,
        provider: &OidcProviderConfig,
        code: &str,
        redirect_uri: &str,
    ) -> Result<VerifiedOidcIdentity, String> {
        let document = self.discovery.fetch_document(&provider.issuer_url).await?;
        if document.issuer.trim_end_matches('/') != provider.issuer_url.trim_end_matches('/') {
            return Err("OIDC discovery issuer does not match configured issuer".to_string());
        }
        let token: TokenResponse = self
            .http
            .post(&document.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("client_id", provider.client_id.as_str()),
                ("client_secret", provider.client_secret.as_str()),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(|error| format!("OIDC token exchange failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("OIDC token endpoint rejected the code: {error}"))?
            .json()
            .await
            .map_err(|error| format!("OIDC token response is invalid: {error}"))?;
        let id_token = token
            .id_token
            .ok_or_else(|| "OIDC token response has no id_token".to_string())?;
        let header = decode_header(&id_token)
            .map_err(|error| format!("OIDC id_token header is invalid: {error}"))?;
        if !matches!(
            header.alg,
            Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::ES256
                | Algorithm::ES384
                | Algorithm::EdDSA
        ) {
            return Err("OIDC id_token uses an unsupported signing algorithm".to_string());
        }
        let jwks: JwkSet = self
            .http
            .get(&document.jwks_uri)
            .send()
            .await
            .map_err(|error| format!("OIDC JWKS request failed: {error}"))?
            .error_for_status()
            .map_err(|error| format!("OIDC JWKS endpoint failed: {error}"))?
            .json()
            .await
            .map_err(|error| format!("OIDC JWKS response is invalid: {error}"))?;
        let jwk = jwks
            .find(header.kid.as_deref().unwrap_or_default())
            .ok_or_else(|| "OIDC id_token signing key was not found".to_string())?;
        let key = DecodingKey::from_jwk(jwk)
            .map_err(|error| format!("OIDC signing key is invalid: {error}"))?;
        let mut validation = Validation::new(header.alg);
        validation.set_audience(&[provider.client_id.as_str()]);
        validation.set_issuer(&[document.issuer.as_str()]);
        let claims = decode::<IdTokenClaims>(&id_token, &key, &validation)
            .map_err(|error| format!("OIDC id_token verification failed: {error}"))?
            .claims;

        let verified_email = if claims.email_verified == Some(false) {
            None
        } else {
            claims.email
        };
        let mut identity = VerifiedOidcIdentity {
            issuer: claims.iss,
            subject: claims.sub,
            email: verified_email,
            name: claims.name.or(claims.preferred_username),
        };
        if identity.email.is_none()
            && let (Some(endpoint), Some(access_token)) = (
                document.userinfo_endpoint.as_deref(),
                token.access_token.as_deref(),
            )
        {
            let info: UserInfoResponse = self
                .http
                .get(endpoint)
                .bearer_auth(access_token)
                .send()
                .await
                .map_err(|error| format!("OIDC userinfo request failed: {error}"))?
                .error_for_status()
                .map_err(|error| format!("OIDC userinfo endpoint failed: {error}"))?
                .json()
                .await
                .map_err(|error| format!("OIDC userinfo response is invalid: {error}"))?;
            if info.sub != identity.subject {
                return Err("OIDC userinfo subject does not match id_token".to_string());
            }
            identity.email = if info.email_verified == Some(false) {
                None
            } else {
                info.email
            };
            identity.name = identity.name.or(info.name).or(info.preferred_username);
        }
        Ok(identity)
    }

    async fn resolve_user(
        &self,
        provider: &OidcProviderConfig,
        identity: &VerifiedOidcIdentity,
        link_user_id: Option<i64>,
    ) -> Result<OidcExchangedUser, String> {
        let persistence = self.persistence()?;
        if let Some(user_id) = link_user_id {
            self.upsert_identity(provider, identity, user_id).await?;
            return self.load_oidc_user(user_id).await;
        }

        if let Some(user_id) = persistence
            .find_identity_user_id(&identity.issuer, &identity.subject)
            .await?
        {
            persistence
                .touch_identity(
                    &identity.issuer,
                    &identity.subject,
                    &identity.email,
                    &provider_identifier(provider),
                )
                .await?;
            return self.load_oidc_user(user_id).await;
        }

        let email = identity
            .email
            .as_deref()
            .filter(|email| !email.trim().is_empty())
            .ok_or_else(|| "OIDC provider did not return an email address".to_string())?;
        let existing = persistence.find_user_id_by_email(email).await?;
        let user_id = if let Some(id) = existing {
            id
        } else {
            if !provider.allow_signup {
                return Err("OIDC signup is disabled for this provider".to_string());
            }
            let (first_name, last_name) = split_name(identity.name.as_deref().unwrap_or_default());
            persistence
                .create_oidc_user(
                    email,
                    conduit_services::user_service::OIDC_ONLY_PLACEHOLDER,
                    &first_name,
                    &last_name,
                )
                .await?
        };
        self.upsert_identity(provider, identity, user_id).await?;
        self.load_oidc_user(user_id).await
    }

    async fn upsert_identity(
        &self,
        provider: &OidcProviderConfig,
        identity: &VerifiedOidcIdentity,
        user_id: i64,
    ) -> Result<(), String> {
        let affected = self
            .persistence()?
            .upsert_identity(
                &identity.issuer,
                &identity.subject,
                &identity.email,
                &provider_identifier(provider),
                user_id,
            )
            .await?;
        if affected != 1 {
            return Err("OIDC identity is already linked to another user".to_string());
        }
        Ok(())
    }

    async fn load_oidc_user(&self, user_id: i64) -> Result<OidcExchangedUser, String> {
        let row = self
            .persistence()?
            .load_user(user_id)
            .await?
            .ok_or_else(|| "OIDC user not found".to_string())?;
        if row.status != "activated" {
            return Err("OIDC user is not activated".to_string());
        }
        Ok(OidcExchangedUser {
            id: row.id,
            user: json!({
                "id": row.id,
                "email": row.email,
                "status": row.status,
                "preferLanguage": row.prefer_language,
                "firstName": row.first_name,
                "avatar": row.avatar,
                "isOwner": row.is_owner,
                "scopes": row.scopes
            }),
        })
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    iss: String,
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(rename = "exp")]
    _exp: usize,
}

#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

struct VerifiedOidcIdentity {
    issuer: String,
    subject: String,
    email: Option<String>,
    name: Option<String>,
}

fn split_name(name: &str) -> (String, String) {
    let mut parts = name.trim().splitn(2, char::is_whitespace);
    (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    )
}

#[async_trait]
impl OidcService for OidcAdapter {
    /// Go `OIDCService.CountProviders` (oidc.go:338-340): the configured
    /// provider count, NOT the count that successfully discovered.
    fn count_providers(&self) -> usize {
        if self.oidc.enabled {
            self.oidc.providers.len()
        } else {
            0
        }
    }

    /// Go `AuthService.AuthenticateJWTToken` (auth.go:160-189): verify the
    /// HS256 token with the api-auth secret and return the numeric `user_id`
    /// claim. The trait yields only the id (the handler uses it as context
    /// identity), so — unlike Go — no DB user load happens here. The
    /// `decode_hs256` validation already enforces `exp` (jwt.rs:56), matching
    /// Go's implicit `exp` check.
    async fn authenticate_jwt_token(&self, token: &str) -> Result<i64, String> {
        let secret = self.secret().await?;
        let claims = decode_hs256(token, &secret).map_err(|err| err.to_string())?;
        Ok(claims.user_id)
    }

    /// Go `OIDCService.GetProviders` (oidc.go:407-460): map the configured
    /// providers to `ProviderInfo`.
    ///
    /// Field mapping (Go `OIDCProvider` → Rust `OidcProviderConfig`, only where
    /// the Rust config carries the value):
    /// * `id` / `name` / `display_name` ← `name` (Go's `normalize()` defaults
    ///   `display_name` to `name` when blank, oidc.go:114-122).
    /// * `jit_enabled` ← `allow_signup` (closest semantic match: both gate
    ///   auto-provisioning of new users on first login).
    /// * `icon_url` / `button_color` / `oidc_login_only` — no Rust config
    ///   equivalent → Go zero values (empty / false).
    /// * `active` — set true when the provider has the minimum viable config
    ///   (non-empty `issuer_url` + `client_id`). Go's precise `active` flag
    ///   reflects live startup discovery (oidc.go:443-448); without startup
    ///   discovery this heuristic is the honest equivalent.
    ///
    /// `is_linked` enrichment is **DEFER**: Go queries
    /// `OIDCIdentity.Where(UserID(u.ID))` (oidc.go:413-422, 450-454) to flag
    /// providers the current user has already linked. The Rust
    /// [`conduit_db::repo::OidcRepo`] trait exposes only
    /// `find_oidc_identity(identity_id)` (repo/mod.rs:599-614) — there is no
    /// list-by-user method, so the per-user link state cannot be backed without
    /// a new repo query. `is_linked` stays `false` (matching Go's anonymous
    /// path, oidc.go:413 `contexts.GetUser` returns false).
    async fn get_providers(&self, user_id: Option<i64>) -> Vec<ProviderInfo> {
        if !self.oidc.enabled {
            return Vec::new();
        }
        // Load the user's linked OIDC identities (Go oidc.go:413-422, 450-454)
        // and index by issuer for O(1) lookup when building the provider list.
        let mut linked_by_issuer: HashMap<String, (String, String)> = HashMap::new();
        if let (Some(uid), Some(persistence)) = (user_id, &self.persistence)
            && let Ok(identities) = persistence.linked_identities(uid).await
        {
            for identity in identities {
                linked_by_issuer.insert(
                    identity.issuer,
                    (identity.id.to_string(), identity.email.unwrap_or_default()),
                );
            }
        }

        self.oidc
            .providers
            .iter()
            .map(|provider| {
                let id = provider_identifier(provider);
                let issuer = provider_issuer(provider);
                let (is_linked, linked_identity_id, linked_email) =
                    if let Some((identity_id, email)) = linked_by_issuer.get(&issuer) {
                        (true, identity_id.clone(), email.clone())
                    } else {
                        (false, String::new(), String::new())
                    };
                ProviderInfo {
                    id: id.clone(),
                    name: id.clone(),
                    display_name: id,
                    jit_enabled: provider.allow_signup,
                    active: !provider.issuer_url.is_empty() && !provider.client_id.is_empty(),
                    is_linked,
                    linked_identity_id,
                    linked_email,
                    icon_url: String::new(),
                    button_color: String::new(),
                    last_check: 0,
                    oidc_login_only: false,
                }
            })
            .collect()
    }

    /// Go `OIDCService.GetAuthorizeURL` (oidc.go:508-598): resolve the
    /// provider, discover its `authorization_endpoint`, mint + cache the CSRF
    /// `state`, and build the authorize URL.
    ///
    /// Deviations from Go (all documented, none fabricated):
    /// * The "not in the live map → re-discover + rate-limit 60s" branch
    ///   (oidc.go:513-559) collapses to lazy discovery here — the Rust host
    ///   does not pre-seed a live provider map at startup, so every authorize
    ///   URL triggers discovery (cached after first success).
    /// * PKCE is skipped (no `enable_pkce` field in the Rust config).
    /// * `response_type=code` is always set (Go's `AuthCodeURL` default).
    async fn get_authorize_url(
        &self,
        provider: &str,
        base_url: &str,
    ) -> Result<(String, String), String> {
        if !self.oidc.enabled {
            return Err("OIDC is disabled".to_string());
        }
        let provider_config = find_provider(&self.oidc.providers, provider)
            .ok_or_else(|| format!("OIDC provider not found: {provider}"))?;

        let issuer = provider_issuer(provider_config);
        let authorize_endpoint = self.authorize_endpoint_for(&issuer).await?;

        let provider_id = provider_identifier(provider_config);
        let redirect_path = default_redirect_path(&provider_id, self.oidc.providers.len());
        let redirect_uri = absolutize_redirect(&redirect_path, base_url);

        // oidc.go:567-575 — mint + cache the CSRF state. Go uses base64url; the
        // hex-encoded `generate_secret_key()` is documented in the module doc.
        let state = generate_secret_key();
        self.state_cache.set(
            format!("oidc_state:{state}"),
            "1".to_string(),
            self.oidc.state_ttl,
        )?;

        let url = build_authorize_url(
            &authorize_endpoint,
            &provider_config.client_id,
            &redirect_uri,
            &effective_scopes(provider_config),
            &state,
        );
        Ok((url, state))
    }

    /// Go `OIDCService.GetLinkAuthorizeURL` (oidc.go:601-614): build the same
    /// authorize URL, then cache the link intent (`oidc_link_state:{state}` →
    /// user_id) so a later `callback` knows to link rather than log in.
    async fn get_link_authorize_url(
        &self,
        provider: &str,
        base_url: &str,
        user_id: i64,
    ) -> Result<(String, String), String> {
        let (url, state) = self.get_authorize_url(provider, base_url).await?;

        // oidc.go:607-611 — cache the link intent for the authenticated user.
        self.state_cache.set(
            format!("oidc_link_state:{state}"),
            user_id.to_string(),
            self.oidc.state_ttl,
        )?;
        Ok((url, state))
    }

    /// Go `OIDCService.Callback` (oidc.go:616-776) is the full live IdP
    /// round-trip — validate the cached CSRF state, exchange the authorization
    /// code for a token via `oauth2Config.Exchange` (live POST to the token
    /// endpoint), verify the id_token signature against the provider JWKS
    /// (`p.verifier.Verify`), fetch userinfo, then either link the identity or
    /// resolve/provision the user + mint a short-lived exchange code.
    ///
    /// The external endpoints are accessed through reqwest while user and
    /// identity persistence is performed against the configured SQL backend.
    async fn callback(
        &self,
        provider: &str,
        code: &str,
        state: &str,
        base_url: &str,
    ) -> Result<(String, String), String> {
        let csrf = self
            .state_cache
            .consume(&format!("oidc_state:{state}"))?
            .ok_or_else(|| "invalid or expired OIDC state".to_string())?;
        if csrf != "1" {
            return Err("invalid or expired OIDC state".to_string());
        }
        let provider_config = find_provider(&self.oidc.providers, provider)
            .ok_or_else(|| format!("OIDC provider not found: {provider}"))?;
        let provider_id = provider_identifier(provider_config);
        let redirect_path = default_redirect_path(&provider_id, self.oidc.providers.len());
        let redirect_uri = absolutize_redirect(&redirect_path, base_url);
        let identity = self
            .exchange_and_verify(provider_config, code, &redirect_uri)
            .await?;
        let link_user_id = self
            .state_cache
            .consume(&format!("oidc_link_state:{state}"))?
            .map(|value| value.parse::<i64>())
            .transpose()
            .map_err(|_| "invalid OIDC link state".to_string())?;
        let user = self
            .resolve_user(provider_config, &identity, link_user_id)
            .await?;
        if link_user_id.is_some() {
            return Ok((String::new(), "link".to_string()));
        }
        let exchange_code = generate_secret_key();
        self.state_cache.set(
            format!("oidc_exchange:{exchange_code}"),
            user.id.to_string(),
            self.oidc.state_ttl,
        )?;
        Ok((exchange_code, "login".to_string()))
    }

    /// Go `OIDCService.ExchangeCode` (oidc.go:1252-1297) consumes the
    /// short-lived exchange code cached by `Callback` and loads the user. It is
    /// strictly downstream of `callback` and is deliberately one-shot.
    async fn exchange_code(&self, code: &str) -> Result<OidcExchangedUser, String> {
        let user_id = self
            .state_cache
            .consume(&format!("oidc_exchange:{code}"))?
            .ok_or_else(|| "invalid or expired exchange code".to_string())?
            .parse::<i64>()
            .map_err(|_| "invalid or expired exchange code".to_string())?;
        self.load_oidc_user(user_id).await
    }

    /// Go `AuthService.GenerateJWTToken` (auth.go:100-119): mint an HS256 JWT
    /// for the exchanged user id with the api-auth secret. Go's claims are
    /// `{user_id, exp(+7d)}`; the Rust `Claims::new` adds `iat` + a
    /// `session_scope` (required by `decode_hs256`'s `Claims` struct, jwt.rs:
    /// 8-19). The `session_scope` follows the `"user:{id}"` convention used by
    /// the oidc_handlers test mock (oidc_handlers.rs:869) and the admin guard.
    /// `exp` is `DEFAULT_JWT_TTL` = 7 days (jwt.rs:6), matching Go.
    async fn generate_jwt_token(&self, user: &OidcExchangedUser) -> Result<String, String> {
        let secret = self.secret().await?;
        let claims = Claims::new(user.id, format!("user:{}", user.id));
        encode_hs256(&claims, &secret).map_err(|err| err.to_string())
    }
}

fn decode_hex_secret(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("stored JWT secret has invalid hex length".to_string());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| "stored JWT secret is not valid hex".to_string())
        })
        .collect()
}

// ===========================================================================
// Tests — JWT round-trip, provider listing from seeded config, authorize-url
// golden (query params vs Go's oauth2.Config.AuthCodeURL construction). The
// only live-network seam (discovery) is mocked; everything else exercises the
// REAL adapter. Result-returning, no unwrap/expect.
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const TEST_ED25519_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIJJ3BQEuwX2HXpzD+vOAGnXEAp6mAUS9cHtIn66k9xTJ\n-----END PRIVATE KEY-----\n";
    const TEST_ED25519_PUBLIC_X: &str = "oNgSgNt8fdVoFEay3s0AFQBQ5A_ZkEzXsCCm_jB2Fe0";
    const TEST_KEY_ID: &str = "oidc-test-key";

    #[derive(Serialize)]
    struct TestIdTokenClaims<'a> {
        iss: &'a str,
        sub: &'a str,
        aud: &'a str,
        exp: u64,
        iat: u64,
        email: &'a str,
        email_verified: bool,
        name: &'a str,
    }

    #[derive(Clone)]
    struct StaticDiscovery {
        document: OidcDiscoveryDocument,
    }

    #[async_trait]
    impl OidcDiscovery for StaticDiscovery {
        async fn fetch_authorize_endpoint(&self, _issuer_url: &str) -> Result<String, String> {
            Ok(self.document.authorization_endpoint.clone())
        }

        async fn fetch_document(&self, _issuer_url: &str) -> Result<OidcDiscoveryDocument, String> {
            Ok(self.document.clone())
        }
    }

    fn signed_test_id_token(
        issuer: &str,
        subject: &str,
        email: &str,
    ) -> Result<String, jsonwebtoken::errors::Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(TEST_KEY_ID.to_string());
        encode(
            &header,
            &TestIdTokenClaims {
                iss: issuer,
                sub: subject,
                aud: "oidc-client",
                exp: now + 300,
                iat: now,
                email,
                email_verified: true,
                name: "Ada Lovelace",
            },
            &EncodingKey::from_ed_pem(TEST_ED25519_PRIVATE_KEY.as_bytes())?,
        )
    }

    async fn fake_idp_adapter_with_persistence(
        persistence: Arc<dyn OidcPersistence>,
        subject: &str,
        email: &str,
    ) -> Result<(OidcAdapter, MockServer), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        let issuer = server.uri();
        let id_token = signed_test_id_token(&issuer, subject, email)?;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fake-access-token",
                "id_token": id_token
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "keys": [{
                    "kty": "OKP",
                    "crv": "Ed25519",
                    "x": TEST_ED25519_PUBLIC_X,
                    "kid": TEST_KEY_ID,
                    "use": "sig",
                    "alg": "EdDSA"
                }]
            })))
            .mount(&server)
            .await;
        let provider = OidcProviderConfig {
            name: "fake-idp".to_string(),
            issuer_url: issuer.clone(),
            client_id: "oidc-client".to_string(),
            client_secret: "oidc-secret".to_string(),
            scopes: vec!["openid".to_string(), "email".to_string()],
            allow_signup: true,
        };
        let discovery = StaticDiscovery {
            document: OidcDiscoveryDocument {
                issuer: issuer.clone(),
                authorization_endpoint: format!("{issuer}/authorize"),
                token_endpoint: format!("{issuer}/token"),
                jwks_uri: format!("{issuer}/jwks"),
                userinfo_endpoint: Some(format!("{issuer}/userinfo")),
            },
        };
        let mut adapter = OidcAdapter::with_discovery(
            OidcConfig {
                enabled: true,
                redirect_base_url: None,
                state_ttl: Duration::from_secs(600),
                providers: vec![provider],
            },
            Some(SECRET.to_string()),
            Box::new(discovery),
        );
        adapter.persistence = Some(persistence);
        Ok((adapter, server))
    }

    /// Mock discovery seam: returns a canned `authorization_endpoint` when the
    /// issuer URL contains `key`, and counts how many times it was consulted
    /// (so the discovery-cache test can assert a single network hit). No live
    /// network — mirrors the http crate's InMemory mock style.
    struct CannedDiscovery {
        key: String,
        endpoint: String,
        calls: std::sync::Arc<Mutex<u32>>,
    }

    impl CannedDiscovery {
        fn new(key: &str, endpoint: &str) -> Self {
            Self {
                key: key.to_string(),
                endpoint: endpoint.to_string(),
                calls: std::sync::Arc::new(Mutex::new(0)),
            }
        }

        /// Handle to the internal call counter (clone before moving the mock
        /// into the adapter).
        fn call_count(&self) -> std::sync::Arc<Mutex<u32>> {
            std::sync::Arc::clone(&self.calls)
        }
    }

    #[async_trait]
    impl OidcDiscovery for CannedDiscovery {
        async fn fetch_authorize_endpoint(&self, issuer_url: &str) -> Result<String, String> {
            if let Ok(mut count) = self.calls.lock() {
                *count += 1;
            }
            if issuer_url.contains(&self.key) {
                Ok(self.endpoint.clone())
            } else {
                Err(format!("no canned endpoint for issuer {issuer_url}"))
            }
        }
    }

    /// Build an adapter whose discovery returns a fixed authorize endpoint per
    /// issuer (no network). Mirrors the http crate's InMemory mock style.
    fn adapter_with_providers(
        providers: Vec<OidcProviderConfig>,
        jwt_secret: Option<&str>,
    ) -> OidcAdapter {
        let oidc = OidcConfig {
            enabled: true,
            redirect_base_url: None,
            state_ttl: Duration::from_secs(10 * 60),
            providers,
        };
        let discovery = Box::new(CannedDiscovery::new(
            "google",
            "https://accounts.google.com/o/oauth2/v2/auth",
        ));
        OidcAdapter::with_discovery(oidc, jwt_secret.map(str::to_string), discovery)
    }

    fn google_provider() -> OidcProviderConfig {
        OidcProviderConfig {
            name: "google".to_string(),
            issuer_url: "https://accounts.google.com".to_string(),
            client_id: "g-client-id".to_string(),
            client_secret: "g-secret".to_string(),
            scopes: Vec::new(), // exercises the openid/profile/email default
            allow_signup: true,
        }
    }

    const SECRET: &str = "oidc-adapter-test-secret";

    // ---- authenticate_jwt_token / generate_jwt_token (REAL pair) -----------

    /// auth.go:100-119 + auth.go:160-189 — a token minted by `generate_jwt_token`
    /// round-trips through `authenticate_jwt_token` to the same user id.
    #[tokio::test]
    async fn jwt_round_trip_recovers_user_id() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let user = OidcExchangedUser {
            id: 42,
            user: serde_json::json!({"id": 42}),
        };
        let token = adapter.generate_jwt_token(&user).await?;
        assert_eq!(adapter.authenticate_jwt_token(&token).await?, 42);
        Ok(())
    }

    /// A tampered token is rejected (auth.go:171-174 signing-method check /
    /// HS256 signature mismatch).
    #[tokio::test]
    async fn authenticate_rejects_tampered_token() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let user = OidcExchangedUser {
            id: 7,
            user: serde_json::json!({"id": 7}),
        };
        let mut token = adapter.generate_jwt_token(&user).await?;
        // Flip the last character of the signature segment.
        let last = token.len() - 1;
        let replacement = if token.as_bytes()[last] == b'A' {
            b'B'
        } else {
            b'A'
        };
        token.replace_range(last.., std::str::from_utf8(&[replacement])?);
        assert!(adapter.authenticate_jwt_token(&token).await.is_err());
        Ok(())
    }

    /// A token signed with a different secret fails verification (the api-auth
    /// secret is the single source of truth).
    #[tokio::test]
    async fn authenticate_rejects_wrong_secret_token() -> TestResult {
        let mint_adapter = adapter_with_providers(vec![google_provider()], Some("other-secret"));
        let verify_adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let user = OidcExchangedUser {
            id: 9,
            user: serde_json::json!({"id": 9}),
        };
        let token = mint_adapter.generate_jwt_token(&user).await?;
        assert!(verify_adapter.authenticate_jwt_token(&token).await.is_err());
        Ok(())
    }

    /// With no secret configured, both mint and verify error out (wiring gap).
    #[tokio::test]
    async fn missing_secret_errors_on_both_directions() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], None);
        let user = OidcExchangedUser {
            id: 1,
            user: serde_json::json!({"id": 1}),
        };
        assert!(adapter.generate_jwt_token(&user).await.is_err());
        assert!(adapter.authenticate_jwt_token("any-token").await.is_err());
        Ok(())
    }

    // ---- count_providers / get_providers (REAL) ----------------------------

    /// oidc.go:338-340 — count mirrors the configured provider list length.
    #[tokio::test]
    async fn count_providers_reflects_configured_list() -> TestResult {
        let single = adapter_with_providers(vec![google_provider()], Some(SECRET));
        assert_eq!(single.count_providers(), 1);

        let mut second = google_provider();
        second.name = "github".to_string();
        let multi = adapter_with_providers(vec![google_provider(), second], Some(SECRET));
        assert_eq!(multi.count_providers(), 2);
        Ok(())
    }

    /// oidc.go:407-460 — provider list maps config → ProviderInfo with the
    /// documented field mapping (id/name/display_name ← name, jit_enabled ←
    /// allow_signup, active when issuer+client_id present). is_linked is false
    /// for both anonymous and identified requests (DEFER enrichment).
    #[tokio::test]
    async fn get_providers_maps_config_to_provider_info() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));

        let anonymous = adapter.get_providers(None).await;
        assert_eq!(anonymous.len(), 1);
        assert_eq!(anonymous[0].id, "google");
        assert_eq!(anonymous[0].name, "google");
        assert_eq!(anonymous[0].display_name, "google");
        assert!(anonymous[0].jit_enabled);
        assert!(anonymous[0].active);
        assert!(!anonymous[0].is_linked);

        // The user_id argument is accepted but is_linked stays false (DEFER).
        let identified = adapter.get_providers(Some(5)).await;
        assert!(!identified[0].is_linked);
        Ok(())
    }

    /// A provider missing issuer/client_id is reported inactive (the minimum
    /// viable config heuristic).
    #[tokio::test]
    async fn get_providers_marks_incomplete_config_inactive() -> TestResult {
        let incomplete = OidcProviderConfig {
            name: "broken".to_string(),
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scopes: Vec::new(),
            allow_signup: false,
        };
        let adapter = adapter_with_providers(vec![incomplete], Some(SECRET));
        let providers = adapter.get_providers(None).await;
        assert!(!providers[0].active);
        assert!(!providers[0].jit_enabled);
        Ok(())
    }

    // ---- get_authorize_url (REAL — golden query params) -------------------

    /// oidc.go:508-598 / oauth2.Config.AuthCodeURL — the authorize URL carries
    /// the discovered endpoint + the Go url.Values.Encode-sorted query (alpha
    /// order: client_id, redirect_uri, response_type, scope, state). The state
    /// is cached for CSRF and the redirect_uri is absolutized against base_url.
    #[tokio::test]
    async fn authorize_url_builds_go_sorted_query_and_caches_state() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let (url, state) = adapter
            .get_authorize_url("google", "https://gateway.example")
            .await?;

        // Discovered endpoint is the URL base.
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );

        let query = url.split('?').nth(1).ok_or("no query string")?;
        // Go url.Values.Encode sorts keys alphabetically.
        let pairs: Vec<&str> = query.split('&').collect();
        let keys: Vec<&str> = pairs
            .iter()
            .map(|pair| pair.split('=').next().unwrap_or(""))
            .collect();
        assert_eq!(
            keys,
            vec![
                "client_id",
                "redirect_uri",
                "response_type",
                "scope",
                "state"
            ],
            "{query}"
        );

        // Values are url.QueryEscape'd.
        assert!(query.contains("response_type=code"), "{query}");
        assert!(query.contains("client_id=g-client-id"), "{query}");
        // Single provider → /oauth/oidc/callback, absolutized with base_url.
        assert!(
            query.contains("redirect_uri=https%3A%2F%2Fgateway.example%2Foauth%2Foidc%2Fcallback"),
            "{query}"
        );
        // Default scopes (openid profile email) are space-joined then escaped.
        assert!(query.contains("scope=openid+profile+email"), "{query}");
        // State is the opaque cached token.
        assert!(
            query.contains(&format!("state={state}")),
            "{query} state={state}"
        );

        // The CSRF state was cached — consume it like Callback would.
        let cached = adapter
            .state_cache
            .consume(&format!("oidc_state:{state}"))
            .map_err(|err| err.to_string())?;
        assert_eq!(cached.as_deref(), Some("1"));
        Ok(())
    }

    /// oidc.go:527-530 — with >1 provider the redirect path is
    /// `/oauth/oidc/callback/{id}` (per-provider callback route).
    #[tokio::test]
    async fn authorize_url_multi_provider_redirect_path() -> TestResult {
        let mut github = google_provider();
        github.name = "github".to_string();
        github.issuer_url = "https://github.com".to_string();
        let discovery = Box::new(CannedDiscovery::new(
            "github",
            "https://github.com/login/oauth/authorize",
        ));
        let oidc = OidcConfig {
            enabled: true,
            redirect_base_url: None,
            state_ttl: Duration::from_secs(10 * 60),
            providers: vec![google_provider(), github],
        };
        let adapter = OidcAdapter::with_discovery(oidc, Some(SECRET.to_string()), discovery);

        let (url, _) = adapter
            .get_authorize_url("github", "https://gateway.example")
            .await?;
        assert!(
            url.contains(
                "redirect_uri=https%3A%2F%2Fgateway.example%2Foauth%2Foidc%2Fcallback%2Fgithub"
            ),
            "{url}"
        );
        Ok(())
    }

    /// oidc.go:513-515 — unknown provider → "OIDC provider not found" error.
    #[tokio::test]
    async fn authorize_url_unknown_provider_errors() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let result = adapter
            .get_authorize_url("ghost", "https://gateway.example")
            .await;
        match result {
            Err(message) => assert!(message.contains("not found"), "{message}"),
            Ok((url, _)) => return Err(format!("expected error, got url {url}").into()),
        }
        Ok(())
    }

    /// Provider matching is case/space-insensitive (oidc.go:141-158
    /// normalizeOIDCProviderIdentifier / matchesIdentifier).
    #[tokio::test]
    async fn authorize_url_matches_provider_case_insensitively() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let (url, _) = adapter.get_authorize_url(" Google ", "https://gw").await?;
        assert!(url.contains("client_id=g-client-id"), "{url}");
        Ok(())
    }

    /// Discovery is cached: the second authorize call for the same issuer does
    /// not hit the discovery seam again.
    #[tokio::test]
    async fn authorize_url_caches_discovery_per_issuer() -> TestResult {
        let discovery = Box::new(CannedDiscovery::new(
            "google",
            "https://accounts.google.com/o/oauth2/v2/auth",
        ));
        let call_count = discovery.call_count();
        let adapter = OidcAdapter::with_discovery(
            OidcConfig {
                enabled: true,
                redirect_base_url: None,
                state_ttl: Duration::from_secs(10 * 60),
                providers: vec![google_provider()],
            },
            Some(SECRET.to_string()),
            discovery,
        );

        adapter.get_authorize_url("google", "https://gw").await?;
        adapter.get_authorize_url("google", "https://gw").await?;

        // Only the first call hit discovery; the second used the cache.
        assert_eq!(*call_count.lock().map_err(|_| "poisoned")?, 1);
        Ok(())
    }

    // ---- get_link_authorize_url (REAL) ------------------------------------

    /// oidc.go:601-614 — the link authorize URL is the same as the login one,
    /// and the link intent (user id) is cached under oidc_link_state:{state}.
    #[tokio::test]
    async fn link_authorize_url_caches_user_intent() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let (url, state) = adapter
            .get_link_authorize_url("google", "https://gateway.example", 11)
            .await?;

        // Same URL shape as a login authorize.
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );
        assert!(url.contains("state="), "{url}");

        // The link intent was cached for the user.
        let intent = adapter
            .state_cache
            .consume(&format!("oidc_link_state:{state}"))
            .map_err(|err| err.to_string())?;
        assert_eq!(intent.as_deref(), Some("11"));
        Ok(())
    }

    // ---- callback / exchange / provisioning (real DB + fake external IdP) -

    #[tokio::test]
    async fn postgres_callback_provisions_and_exchanges_when_dsn_is_provided() -> TestResult {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let subject = format!("pg-subject-{suffix}");
        let email = format!("pg-oidc-{suffix}@example.test");
        let (adapter, server) = fake_idp_adapter_with_persistence(
            Arc::new(PostgresOidcPersistence::new(pool.clone())),
            &subject,
            &email,
        )
        .await?;
        let (_, state) = adapter
            .get_authorize_url("fake-idp", "https://gateway.example")
            .await?;
        let (exchange_code, intent) = adapter
            .callback(
                "fake-idp",
                "postgres-authorization-code",
                &state,
                "https://gateway.example",
            )
            .await?;
        assert_eq!(intent, "login");
        let exchanged = adapter.exchange_code(&exchange_code).await?;
        assert_eq!(exchanged.user["email"], email);
        assert!(adapter.exchange_code(&exchange_code).await.is_err());

        let identity_owner: i64 = sqlx::query_scalar(
            "SELECT user_id FROM oidc_identities \
             WHERE issuer = $1 AND subject = $2 AND deleted_at = 0",
        )
        .bind(server.uri())
        .bind(&subject)
        .fetch_one(&pool)
        .await?;
        assert_eq!(identity_owner, exchanged.id);
        let providers = adapter.get_providers(Some(exchanged.id)).await;
        assert!(providers[0].is_linked);
        assert_eq!(providers[0].linked_email, email);
        let token = adapter.generate_jwt_token(&exchanged).await?;
        assert_eq!(adapter.authenticate_jwt_token(&token).await?, exchanged.id);

        sqlx::query("DELETE FROM oidc_identities WHERE user_id = $1")
            .bind(exchanged.id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id = $1 AND email = $2")
            .bind(exchanged.id)
            .bind(&email)
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn disabled_oidc_exposes_no_provider_or_authorize_flow() -> TestResult {
        let mut adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        adapter.oidc.enabled = false;
        assert_eq!(adapter.count_providers(), 0);
        assert!(adapter.get_providers(None).await.is_empty());
        let error = adapter
            .get_authorize_url("google", "https://gateway.example")
            .await
            .err()
            .ok_or("disabled OIDC unexpectedly produced an authorize URL")?;
        assert_eq!(error, "OIDC is disabled");
        Ok(())
    }

    /// Callback rejects an unknown/expired one-shot state before any IdP call.
    #[tokio::test]
    async fn callback_rejects_unknown_state() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let result = adapter
            .callback("google", "code", "state", "https://gw")
            .await;
        match result {
            Err(message) => assert!(message.contains("invalid or expired"), "{message}"),
            Ok(pair) => return Err(format!("expected state error, got {pair:?}").into()),
        }
        Ok(())
    }

    /// Exchange codes are one-shot and reject unknown values.
    #[tokio::test]
    async fn exchange_code_rejects_unknown_code() -> TestResult {
        let adapter = adapter_with_providers(vec![google_provider()], Some(SECRET));
        let result = adapter.exchange_code("any-code").await;
        match result {
            Err(message) => assert!(message.contains("invalid or expired"), "{message}"),
            Ok(user) => return Err(format!("expected exchange-code error, got {user:?}").into()),
        }
        Ok(())
    }

    // ---- pure helper golden cases -----------------------------------------

    /// encode_query mirrors Go url.Values.Encode (sorted keys + QueryEscape).
    #[test]
    fn encode_query_sorts_and_escapes_like_go() {
        assert_eq!(
            encode_query(&[
                ("state", "z"),
                ("client_id", "a b"),
                ("scope", "openid profile")
            ]),
            "client_id=a+b&scope=openid+profile&state=z"
        );
    }

    /// default_redirect_path: single → /oauth/oidc/callback; multi → per-id.
    #[test]
    fn redirect_path_matches_go_resolution() {
        assert_eq!(default_redirect_path("google", 1), "/oauth/oidc/callback");
        assert_eq!(
            default_redirect_path("google", 2),
            "/oauth/oidc/callback/google"
        );
    }

    /// build_authorize_url produces the exact Go oauth2 AuthCodeURL shape
    /// (endpoint + '?' + sorted query).
    #[test]
    fn build_authorize_url_golden() {
        let url = build_authorize_url(
            "https://idp.example/authorize",
            "cid",
            "https://gw/callback",
            &["openid".to_string(), "email".to_string()],
            "st",
        );
        assert_eq!(
            url,
            "https://idp.example/authorize?\
             client_id=cid&redirect_uri=https%3A%2F%2Fgw%2Fcallback&\
             response_type=code&scope=openid+email&state=st"
        );
    }

    /// A provider list with a config carrying explicit scopes uses them
    /// verbatim rather than the default triple.
    #[test]
    fn effective_scopes_respects_explicit_config() {
        let provider = OidcProviderConfig {
            name: "custom".to_string(),
            issuer_url: "https://idp".to_string(),
            client_id: "c".to_string(),
            client_secret: String::new(),
            scopes: vec!["openid".to_string(), "custom:scope".to_string()],
            allow_signup: false,
        };
        assert_eq!(
            effective_scopes(&provider),
            vec!["openid".to_string(), "custom:scope".to_string()]
        );
    }

    // A serde sanity check: ProviderInfo with only the REAL-set fields
    // serializes to the Go snake_case shape (parity with oidc_handlers tests).
    #[test]
    fn provider_info_serializes_go_shape() -> Result<(), serde_json::Error> {
        let info = ProviderInfo {
            id: "google".to_string(),
            name: "google".to_string(),
            display_name: "google".to_string(),
            jit_enabled: true,
            active: true,
            ..ProviderInfo::default()
        };
        let value: Value = serde_json::to_value(&info)?;
        assert_eq!(value["id"], "google");
        assert_eq!(value["display_name"], "google");
        assert_eq!(value["jit_enabled"], true);
        assert_eq!(value["active"], true);
        assert_eq!(value["is_linked"], false);
        // omitempty fields absent when zero.
        assert!(value.get("last_check").is_none());
        assert!(value.get("linked_identity_id").is_none());
        Ok(())
    }
}

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use rand::{Rng, distributions::Alphanumeric};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub type OidcServiceResult<T> = Result<T, OidcServiceError>;

const PKCE_VERIFIER_LEN: usize = 64;
const STATE_VALUE_LEN: usize = 32;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OidcServiceError {
    #[error("oidc provider is disabled: {0}")]
    ProviderDisabled(String),
    #[error("oidc provider not found: {0}")]
    ProviderNotFound(String),
    #[error("invalid oidc provider config for {provider}: {reason}")]
    InvalidProviderConfig { provider: String, reason: String },
    #[error("oidc state persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcProviderConfig {
    pub name: String,
    pub enabled: bool,
    pub issuer_url: String,
    pub authorization_endpoint: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub allow_signup: bool,
}

impl Default for OidcProviderConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: false,
            issuer_url: String::new(),
            authorization_endpoint: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: String::new(),
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            allow_signup: true,
        }
    }
}

impl OidcProviderConfig {
    pub fn validate(&self) -> OidcServiceResult<()> {
        if !self.enabled {
            return Err(OidcServiceError::ProviderDisabled(self.name.clone()));
        }

        require_non_empty(&self.name, "name", &self.name)?;
        require_http_url(&self.name, "issuer_url", &self.issuer_url)?;
        require_http_url(
            &self.name,
            "authorization_endpoint",
            &self.authorization_endpoint,
        )?;
        require_non_empty(&self.name, "client_id", &self.client_id)?;
        require_non_empty(&self.name, "client_secret", &self.client_secret)?;
        require_http_url(&self.name, "redirect_uri", &self.redirect_uri)?;

        if !self.scopes.iter().any(|scope| scope == "openid") {
            return Err(OidcServiceError::InvalidProviderConfig {
                provider: self.name.clone(),
                reason: "scopes must include openid".to_string(),
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkceChallenge {
    pub verifier: String,
    pub challenge: String,
    pub method: String,
}

impl PkceChallenge {
    pub fn generate() -> Self {
        let verifier = random_token(PKCE_VERIFIER_LEN);
        let digest = Sha256::digest(verifier.as_bytes());

        Self {
            verifier,
            challenge: base64_url_no_pad(&digest),
            method: "S256".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcState {
    pub value: String,
    pub provider_name: String,
    pub redirect_uri: String,
    pub pkce_verifier: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl OidcState {
    pub fn new(
        provider_name: impl Into<String>,
        redirect_uri: impl Into<String>,
        pkce_verifier: impl Into<String>,
        ttl: Duration,
    ) -> Self {
        let created_at = Utc::now();
        let expires_at = created_at
            + chrono::Duration::from_std(ttl).unwrap_or_else(|_| {
                chrono::Duration::seconds(i64::MAX / chrono::Duration::seconds(1).num_seconds())
            });

        Self {
            value: random_token(STATE_VALUE_LEN),
            provider_name: provider_name.into(),
            redirect_uri: redirect_uri.into(),
            pkce_verifier: pkce_verifier.into(),
            created_at,
            expires_at,
        }
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizationRequest {
    pub authorization_url: String,
    pub provider: OidcProviderConfig,
    pub state: OidcState,
    pub pkce: PkceChallenge,
}

#[async_trait]
pub trait OidcStateRepo: Send + Sync {
    async fn store_state(&self, ctx: &RequestContext, state: OidcState) -> OidcServiceResult<()>;

    async fn consume_state(
        &self,
        ctx: &RequestContext,
        state_value: &str,
    ) -> OidcServiceResult<Option<OidcState>>;
}

pub struct OidcService {
    providers: BTreeMap<String, OidcProviderConfig>,
    state_repo: Arc<dyn OidcStateRepo>,
    state_ttl: Duration,
}

impl OidcService {
    pub fn new(
        providers: Vec<OidcProviderConfig>,
        state_repo: Arc<dyn OidcStateRepo>,
        state_ttl: Duration,
    ) -> OidcServiceResult<Self> {
        let mut indexed = BTreeMap::new();

        for provider in providers {
            if provider.name.trim().is_empty() {
                return Err(OidcServiceError::InvalidProviderConfig {
                    provider: provider.name,
                    reason: "name is required".to_string(),
                });
            }
            indexed.insert(provider.name.clone(), provider);
        }

        Ok(Self {
            providers: indexed,
            state_repo,
            state_ttl,
        })
    }

    pub fn provider(&self, provider_name: &str) -> OidcServiceResult<&OidcProviderConfig> {
        let provider = self
            .providers
            .get(provider_name)
            .ok_or_else(|| OidcServiceError::ProviderNotFound(provider_name.to_string()))?;
        provider.validate()?;
        Ok(provider)
    }

    pub async fn authorization_request(
        &self,
        ctx: &RequestContext,
        provider_name: &str,
    ) -> OidcServiceResult<AuthorizationRequest> {
        let provider = self.provider(provider_name)?.clone();
        let pkce = PkceChallenge::generate();
        let state = OidcState::new(
            provider.name.clone(),
            provider.redirect_uri.clone(),
            pkce.verifier.clone(),
            self.state_ttl,
        );
        let authorization_url = authorization_url(&provider, &state.value, &pkce);

        self.state_repo.store_state(ctx, state.clone()).await?;

        Ok(AuthorizationRequest {
            authorization_url,
            provider,
            state,
            pkce,
        })
    }

    pub async fn consume_state(
        &self,
        ctx: &RequestContext,
        state_value: &str,
    ) -> OidcServiceResult<Option<OidcState>> {
        let state = self.state_repo.consume_state(ctx, state_value).await?;

        Ok(state.filter(|state| !state.is_expired(Utc::now())))
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeOidcStateRepo {
    inner: Arc<Mutex<BTreeMap<String, OidcState>>>,
}

impl FakeOidcStateRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state_count(&self) -> OidcServiceResult<usize> {
        Ok(self.lock()?.len())
    }

    fn lock(&self) -> OidcServiceResult<std::sync::MutexGuard<'_, BTreeMap<String, OidcState>>> {
        self.inner
            .lock()
            .map_err(|_| OidcServiceError::LockPoisoned)
    }
}

#[async_trait]
impl OidcStateRepo for FakeOidcStateRepo {
    async fn store_state(&self, _ctx: &RequestContext, state: OidcState) -> OidcServiceResult<()> {
        self.lock()?.insert(state.value.clone(), state);
        Ok(())
    }

    async fn consume_state(
        &self,
        _ctx: &RequestContext,
        state_value: &str,
    ) -> OidcServiceResult<Option<OidcState>> {
        Ok(self.lock()?.remove(state_value))
    }
}

fn authorization_url(
    provider: &OidcProviderConfig,
    state_value: &str,
    pkce: &PkceChallenge,
) -> String {
    let scope = provider.scopes.join(" ");
    let params = [
        ("response_type", "code"),
        ("client_id", provider.client_id.as_str()),
        ("redirect_uri", provider.redirect_uri.as_str()),
        ("scope", scope.as_str()),
        ("state", state_value),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", pkce.method.as_str()),
    ];
    let query = params
        .iter()
        .map(|(name, value)| format!("{}={}", percent_encode(name), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{}?{}", provider.authorization_endpoint, query)
}

fn require_non_empty(provider: &str, field: &'static str, value: &str) -> OidcServiceResult<()> {
    if value.trim().is_empty() {
        return Err(OidcServiceError::InvalidProviderConfig {
            provider: provider.to_string(),
            reason: format!("{field} is required"),
        });
    }

    Ok(())
}

fn require_http_url(provider: &str, field: &'static str, value: &str) -> OidcServiceResult<()> {
    require_non_empty(provider, field, value)?;

    if !(value.starts_with("https://") || value.starts_with("http://")) {
        return Err(OidcServiceError::InvalidProviderConfig {
            provider: provider.to_string(),
            reason: format!("{field} must be an http(s) URL"),
        });
    }

    Ok(())
}

fn random_token(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity((bytes.len() * 4).div_ceil(3));

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let combined = ((first as u32) << 16) | ((second as u32) << 8) | third as u32;

        encoded.push(TABLE[((combined >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((combined >> 12) & 0x3f) as usize] as char);

        if chunk.len() > 1 {
            encoded.push(TABLE[((combined >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(combined & 0x3f) as usize] as char);
        }
    }

    encoded
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();

    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }

    encoded
}

// ============================================================================
// RUST-P5-002 / RUST-P11-003 — pure OIDC logic (Go biz/oidc.go + api/oidc.go)
// ============================================================================
//
// These four pure helpers mirror the Go contract without touching IO:
//
// 1. `validate_oidc_provider_config` — S07 provider config validation
//    (biz/oidc.go:206-219, 261-264, 277-293).
// 2. `extract_callback_params` — S11 callback extract
//    (api/oidc.go:144-156).
// 3. `callback_intent` — link-vs-login intent encoded in the state token
//    (biz/oidc.go:601-614, 737-753).
// 4. `verify_pkce` — RFC 7636 S256 challenge/verifier pairing
//    (biz/oidc.go:592 mirrors `oauth2.S256ChallengeFromVerifier`).

/// Reason a provider config failed validation. Mirrors the Go errors emitted
/// from `NewOIDCService` (biz/oidc.go:211, 216, 261-264).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderConfigError {
    /// "OIDC provider at index {idx} requires id or name"
    /// (biz/oidc.go:211).
    #[error("OIDC provider at index {index} requires id or name")]
    MissingIdOrName { index: usize },
    /// "duplicate OIDC provider id {id} conflicts with {previous}"
    /// (biz/oidc.go:216).
    #[error("duplicate OIDC provider id {id} conflicts with {previous}")]
    DuplicateProviderId {
        id: String,
        previous: String,
        index: usize,
    },
    /// "OIDC provider {id} missing required endpoints (discovery failed and no
    /// manual endpoints provided)" (biz/oidc.go:262). When `client_id` is
    /// missing the Go code silently registers a broken provider; we surface it
    /// explicitly so the pure helper is total.
    #[error("OIDC provider {id} missing required field: {field}")]
    MissingRequiredField { id: String, field: &'static str },
}

/// Raw provider fields used by [`validate_oidc_provider_config`]. Mirrors the
/// subset of Go `OIDCProvider` (biz/oidc.go:53-85) that the constructor
/// validates before instantiating the OAuth2 client:
/// `ID`/`Name`/`DisplayName`, `Issuer`/`IssuerURL`, `ClientID`,
/// `AuthURL`/`TokenURL`, `RedirectURL`, `ExtraScopes`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfigInput<'a> {
    pub index: usize,
    pub id: &'a str,
    pub name: &'a str,
    pub display_name: &'a str,
    pub issuer: &'a str,
    pub issuer_url: &'a str,
    pub client_id: &'a str,
    pub auth_url: &'a str,
    pub token_url: &'a str,
    pub redirect_url: &'a str,
    pub extra_scopes: &'a [String],
}

/// Normalizes a raw provider identifier the way Go's `OIDCProvider.normalize`
/// + `providerID` does (biz/oidc.go:100-127).
///
/// - Trims id/name/display_name.
/// - id falls back to name, then to display_name.
/// - name falls back to id; display_name falls back to name.
///
/// Returns `(id, name, display_name)`.
pub fn normalize_provider_idents(
    raw_id: &str,
    raw_name: &str,
    raw_display_name: &str,
) -> (String, String, String) {
    let id = raw_id.trim();
    let name = raw_name.trim();
    let display_name = raw_display_name.trim();

    let resolved_id = if !id.is_empty() {
        id
    } else if !name.is_empty() {
        name
    } else {
        display_name
    }
    .to_string();

    let resolved_name = if name.is_empty() {
        resolved_id.clone()
    } else {
        name.to_string()
    };

    let resolved_display_name = if display_name.is_empty() {
        resolved_name.clone()
    } else {
        display_name.to_string()
    };

    (resolved_id, resolved_name, resolved_display_name)
}

/// Mirrors `normalizeOIDCProviderIdentifier` (biz/oidc.go:141-143):
/// `strings.ToLower(strings.ReplaceAll(strings.TrimSpace(value), " ", ""))`.
pub fn normalize_provider_identifier(value: &str) -> String {
    value.trim().replace(' ', "").to_ascii_lowercase()
}

/// Validates a single OIDC provider config the way Go's `NewOIDCService` loop
/// does (biz/oidc.go:206-264), plus the scope normalization at
/// biz/oidc.go:285-293. Returns the resolved provider id (post-normalization)
/// and the scope list to send to the IdP (defaults to
/// `["openid", "profile", "email"]` when `extra_scopes` is empty, exactly like
/// Go line 287-288).
///
/// This is the pure logic half of the constructor; cache/network side effects
/// stay in [`OidcService`]. `seen_normalized_ids` is mutated in place so a
/// caller iterating over many providers can detect duplicates the same way Go
/// uses `seenProviderIDs` (biz/oidc.go:214-219).
pub fn validate_oidc_provider_config(
    input: &ProviderConfigInput<'_>,
    seen_normalized_ids: &mut BTreeMap<String, String>,
) -> Result<(String, Vec<String>), ProviderConfigError> {
    let (provider_id, _name, _display_name) =
        normalize_provider_idents(input.id, input.name, input.display_name);

    // biz/oidc.go:209-212: "OIDC provider at index %d requires id or name"
    if provider_id.is_empty() {
        return Err(ProviderConfigError::MissingIdOrName { index: input.index });
    }

    // biz/oidc.go:214-219: duplicate-provider check on the *normalized* id.
    let normalized = normalize_provider_identifier(&provider_id);
    if let Some(previous) = seen_normalized_ids.get(&normalized) {
        return Err(ProviderConfigError::DuplicateProviderId {
            id: provider_id,
            previous: previous.clone(),
            index: input.index,
        });
    }
    seen_normalized_ids.insert(normalized, provider_id.clone());

    // biz/oidc.go:233-264: discovery via IssuerURL or manual AuthURL+TokenURL.
    // "Ensure we have enough to proceed" — at least one source of endpoints.
    let has_discovery = !input.issuer_url.is_empty();
    let has_manual_endpoints = !input.auth_url.is_empty() && !input.token_url.is_empty();
    if !has_discovery && !has_manual_endpoints {
        // biz/oidc.go:262: "missing required endpoints (discovery failed and
        // no manual endpoints provided)". Surface as a missing-field error so
        // the pure helper stays total.
        return Err(ProviderConfigError::MissingRequiredField {
            id: provider_id,
            field: "issuer_url or (auth_url and token_url)",
        });
    }

    // biz/oidc.go:290 (ClientID is wired into oauth2.Config unconditionally).
    // Go does not validate it here, but a missing ClientID makes the OAuth2
    // config unusable; surface it explicitly for parity clarity.
    if input.client_id.trim().is_empty() {
        return Err(ProviderConfigError::MissingRequiredField {
            id: provider_id,
            field: "client_id",
        });
    }

    // biz/oidc.go:285-293: scope normalization.
    // "scopes = p.ExtraScopes; if len(scopes) == 0 { scopes = [openid, profile, email] }"
    let scopes = if input.extra_scopes.is_empty() {
        vec![
            "openid".to_string(),
            "profile".to_string(),
            "email".to_string(),
        ]
    } else {
        input.extra_scopes.to_vec()
    };

    Ok((provider_id, scopes))
}

/// Builds the IdP-facing redirect URL for a provider, mirroring biz/oidc.go:277-283.
///
/// - If the provider config supplies a non-empty `redirect_url`, it wins.
/// - Otherwise the default is `/oauth/oidc/callback`, or
///   `/oauth/oidc/callback/{provider_id}` when more than one provider is
///   registered (so each IdP can be routed to the correct handler).
pub fn build_callback_redirect_url(
    configured_redirect: &str,
    provider_id: &str,
    total_provider_count: usize,
) -> String {
    let trimmed = configured_redirect.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }

    if total_provider_count > 1 {
        format!("/oauth/oidc/callback/{provider_id}")
    } else {
        "/oauth/oidc/callback".to_string()
    }
}

/// Callback query parameters extracted from the IdP redirect, mirroring
/// api/oidc.go:144-156.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallbackParams {
    /// `c.Query("code")` — authorization code (api/oidc.go:144).
    pub code: String,
    /// `c.Query("state")` — CSRF state token (api/oidc.go:145).
    pub state: String,
    /// `c.Query("error")` — provider error short code (api/oidc.go:146).
    pub error: String,
    /// `c.Query("error_description")` — human-readable provider error text
    /// (api/oidc.go:149).
    pub error_description: String,
}

/// Reason [`extract_callback_params`] rejected the IdP redirect.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CallbackExtractError {
    /// Provider signalled an error. Mirrors api/oidc.go:148-151
    /// (`if errorDesc != "" { ... error: c.Query("error_description") }`).
    /// The payload is `error_description` (falling back to `error` when the
    /// description is empty, matching the Go response body).
    #[error("provider returned error: {0}")]
    ProviderError(String),
    /// "Code and state are required" — api/oidc.go:153-156.
    #[error("Code and state are required")]
    MissingCodeOrState,
}

/// Extracts the four callback parameters from a parsed query-string map,
/// enforcing the same validation order as api/oidc.go:144-156.
///
/// `query` is a flat `&str -> &str` map (e.g. from `url::form_urlencoded` or a
/// Gin `c.Query` lookup). The function is pure: HTTP parsing happens upstream.
///
/// Order of checks (must match Go):
/// 1. If `error` is non-empty -> `Err(ProviderError(error_description or error))`.
/// 2. If `code` or `state` is empty -> `Err(MissingCodeOrState)`.
/// 3. Otherwise -> `Ok(CallbackParams { ... })`.
pub fn extract_callback_params<I, K, V>(query: I) -> Result<CallbackParams, CallbackExtractError>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    let mut params: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in query {
        params.insert(key.as_ref().to_string(), value.into());
    }

    let lookup = |name: &str| params.get(name).cloned().unwrap_or_default();
    let code = lookup("code");
    let state = lookup("state");
    let error = lookup("error");
    let error_description = lookup("error_description");

    // api/oidc.go:148-151: provider-reported error short-circuits everything.
    if !error.is_empty() {
        // Go responds with `{"error": c.Query("error_description")}`; when the
        // description is empty, fall back to the error code so the caller still
        // sees something useful.
        let message = if error_description.is_empty() {
            error
        } else {
            error_description
        };
        return Err(CallbackExtractError::ProviderError(message));
    }

    // api/oidc.go:153-156: "if code == "" || state == """.
    if code.is_empty() || state.is_empty() {
        return Err(CallbackExtractError::MissingCodeOrState);
    }

    Ok(CallbackParams {
        code,
        state,
        error,
        error_description,
    })
}

/// Whether an OIDC state token represents a link (attach identity to an
/// already-authenticated user) or a login (sign-in) flow.
///
/// Mirrors the Go link-vs-login distinction encoded by the separate cache key
/// `oidc_link_state:<state>` (biz/oidc.go:608, 737-753):
/// - `GetLinkAuthorizeURL` writes `oidc_link_state:<state>` alongside
///   `oidc_state:<state>` (biz/oidc.go:601-614).
/// - `Callback` checks for that key and returns intent `"link"` if present
///   (biz/oidc.go:737-753); otherwise the flow is `"login"` (biz/oidc.go:775).
///
/// In this Rust port, the intent is encoded directly in the state token via
/// the [`OidcStateToken`] scheme so the helper stays pure (no cache lookup).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallbackIntent {
    /// `oidc_link_state:<state>` absent -> biz/oidc.go:775 (`return exchangeCode, "login", nil`).
    Login,
    /// `oidc_link_state:<state>` present -> biz/oidc.go:752 (`return "", "link", nil`).
    Link,
}

/// Prefix marking a state token as a link-flow state. Picked to be safe inside
/// a URL query parameter and unlikely to collide with a real OIDC state.
/// The Go equivalent is the *existence* of cache key `oidc_link_state:<state>`;
/// here we encode it inside the token so the helper needs no cache.
pub const LINK_STATE_PREFIX: &str = "lnk_";

/// Encodes the link/login intent into a state token so [`callback_intent`] can
/// recover it purely. For login flows the token is returned verbatim; for link
/// flows the token gets the [`LINK_STATE_PREFIX`] prepended.
pub fn encode_callback_intent(state_value: &str, intent: CallbackIntent) -> String {
    match intent {
        CallbackIntent::Login => state_value.to_string(),
        CallbackIntent::Link => {
            if state_value.starts_with(LINK_STATE_PREFIX) {
                state_value.to_string()
            } else {
                format!("{LINK_STATE_PREFIX}{state_value}")
            }
        }
    }
}

/// Recovers the link-vs-login intent from a state token, mirroring the Go
/// "is `oidc_link_state:<state>` present?" check (biz/oidc.go:737-738) without
/// touching the cache.
pub fn callback_intent(state_token: &str) -> CallbackIntent {
    if state_token.starts_with(LINK_STATE_PREFIX) {
        CallbackIntent::Link
    } else {
        CallbackIntent::Login
    }
}

/// Verifies a PKCE S256 challenge/verifier pairing, mirroring the Go behaviour
/// at biz/oidc.go:592 where the challenge is derived via
/// `oauth2.S256ChallengeFromVerifier(verifier)` =
/// `base64.RawURLEncoding.EncodeToString(sha256(verifier))`.
///
/// Returns `true` iff `base64url_no_pad(sha256(verifier)) == expected_challenge`.
/// Pure; no IO.
///
/// Matches RFC 7636 Appendix B test vector (verifier
/// `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk` -> challenge
/// `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`).
pub fn verify_pkce(verifier: &str, expected_challenge: &str) -> bool {
    let digest = Sha256::digest(verifier.as_bytes());
    let computed = base64_url_no_pad(&digest);
    // Constant-time-ish comparison: both sides are fixed-length base64url
    // strings derived from SHA-256, so a direct equality is acceptable. Use
    // `bool::from` to avoid short-circuit on first mismatch leaking timing.
    let same = computed
        .as_bytes()
        .iter()
        .zip(expected_challenge.as_bytes());
    let mut mismatch = computed.len() != expected_challenge.len();
    for (a, b) in same {
        mismatch |= a != b;
    }
    !mismatch
}

/// Pure helper computing the S256 challenge from a verifier, mirroring
/// `oauth2.S256ChallengeFromVerifier` / `conduit_http::oidc_helpers::pkce_challenge`.
/// Kept here so `conduit-services` does not need to depend on `conduit-http`
/// for a one-line SHA-256 derivation; the output is byte-for-byte identical.
pub fn pkce_challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64_url_no_pad(&digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_db::{PolicyContext, Principal};

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn provider() -> OidcProviderConfig {
        OidcProviderConfig {
            name: "local".to_string(),
            enabled: true,
            issuer_url: "https://issuer.example".to_string(),
            authorization_endpoint: "https://issuer.example/oauth2/authorize".to_string(),
            client_id: "client-1".to_string(),
            client_secret: "secret-1".to_string(),
            redirect_uri: "https://conduit.example/auth/oidc/callback".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string()],
            allow_signup: true,
        }
    }

    fn service(repo: Arc<FakeOidcStateRepo>, provider: OidcProviderConfig) -> OidcService {
        match OidcService::new(vec![provider], repo, Duration::from_secs(600)) {
            Ok(service) => service,
            Err(error) => panic!("service should build: {error}"),
        }
    }

    #[tokio::test]
    async fn disabled_provider_rejected() {
        let repo = Arc::new(FakeOidcStateRepo::new());
        let mut provider = provider();
        provider.enabled = false;
        let service = service(repo, provider);

        let result = service.authorization_request(&ctx(), "local").await;

        assert!(matches!(
            result,
            Err(OidcServiceError::ProviderDisabled(provider)) if provider == "local"
        ));
    }

    #[test]
    fn pkce_verifier_and_challenge_have_expected_shape() {
        let pkce = PkceChallenge::generate();

        assert_eq!(pkce.verifier.len(), PKCE_VERIFIER_LEN);
        assert!(
            pkce.verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        );
        assert_eq!(pkce.challenge.len(), 43);
        assert!(
            pkce.challenge
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        assert_eq!(pkce.method, "S256");
    }

    #[tokio::test]
    async fn state_consumes_once() -> OidcServiceResult<()> {
        let repo = Arc::new(FakeOidcStateRepo::new());
        let service = service(Arc::clone(&repo), provider());
        let request = service.authorization_request(&ctx(), "local").await?;

        assert_eq!(repo.state_count()?, 1);

        let first = service.consume_state(&ctx(), &request.state.value).await?;
        let second = service.consume_state(&ctx(), &request.state.value).await?;

        assert_eq!(first.map(|state| state.value), Some(request.state.value));
        assert_eq!(second, None);
        assert_eq!(repo.state_count()?, 0);

        Ok(())
    }

    // ========================================================================
    // RUST-P5-002 / RUST-P11-003 S07/S11 — pure OIDC logic
    // ========================================================================

    fn provider_input<'a>(
        id: &'a str,
        issuer_url: &'a str,
        client_id: &'a str,
    ) -> ProviderConfigInput<'a> {
        ProviderConfigInput {
            index: 0,
            id,
            name: id,
            display_name: id,
            issuer: "",
            issuer_url,
            client_id,
            auth_url: "",
            token_url: "",
            redirect_url: "",
            extra_scopes: &[],
        }
    }

    // --- normalize_provider_idents (biz/oidc.go:100-127) --------------------

    #[test]
    fn normalize_falls_back_id_from_name_then_display_name() {
        // biz/oidc.go:107-112: id wins; if empty, name; if empty, display_name.
        assert_eq!(
            normalize_provider_idents("google", "", ""),
            (
                "google".to_string(),
                "google".to_string(),
                "google".to_string()
            )
        );
        assert_eq!(
            normalize_provider_idents("", "github", ""),
            (
                "github".to_string(),
                "github".to_string(),
                "github".to_string()
            )
        );
        assert_eq!(
            normalize_provider_idents("", "", "Acme Corp"),
            (
                "Acme Corp".to_string(),
                "Acme Corp".to_string(),
                "Acme Corp".to_string()
            )
        );
    }

    #[test]
    fn normalize_trims_whitespace_from_all_fields() {
        // biz/oidc.go:102-104: TrimSpace on id/name/display_name.
        assert_eq!(
            normalize_provider_idents("  google  ", "\tgithub\t", " Acme "),
            (
                "google".to_string(),
                "github".to_string(),
                "Acme".to_string()
            )
        );
    }

    #[test]
    fn normalize_empty_returns_empty_triplet() {
        // When id/name/display_name are all empty/whitespace, id stays "" —
        // the caller (validate_oidc_provider_config) then rejects with
        // MissingIdOrName, mirroring biz/oidc.go:209-212.
        let (id, name, display) = normalize_provider_idents("   ", "  ", "");
        assert!(id.is_empty());
        assert!(name.is_empty());
        assert!(display.is_empty());
    }

    // --- normalize_provider_identifier (biz/oidc.go:141-143) ----------------

    #[test]
    fn normalize_identifier_lowercases_and_strips_spaces() {
        // biz/oidc.go:141-143: ToLower(ReplaceAll(TrimSpace(v), " ", "")).
        assert_eq!(
            normalize_provider_identifier("  Google Suite "),
            "googlesuite"
        );
        assert_eq!(normalize_provider_identifier("GitHub"), "github");
        assert_eq!(normalize_provider_identifier("  "), "");
    }

    // --- validate_oidc_provider_config (biz/oidc.go:206-293) ----------------

    #[test]
    fn validate_rejects_provider_without_id_or_name() {
        // biz/oidc.go:209-212: "OIDC provider at index %d requires id or name".
        let input = ProviderConfigInput {
            index: 3,
            id: "   ",
            name: "",
            display_name: "",
            issuer: "",
            issuer_url: "https://idp.example",
            client_id: "c1",
            auth_url: "",
            token_url: "",
            redirect_url: "",
            extra_scopes: &[],
        };
        let mut seen = BTreeMap::new();
        match validate_oidc_provider_config(&input, &mut seen) {
            Err(ProviderConfigError::MissingIdOrName { index }) => {
                assert_eq!(index, 3);
            }
            other => panic!("expected MissingIdOrName, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_duplicate_normalized_id() {
        // biz/oidc.go:214-219: "duplicate OIDC provider id %q conflicts with %q".
        let mut seen = BTreeMap::new();
        let first = provider_input("Google", "https://g", "c1");
        match validate_oidc_provider_config(&first, &mut seen) {
            Ok(_) => {}
            Err(e) => panic!("first provider should be valid: {e:?}"),
        }

        // Different surface spelling but same normalized id ("google").
        let dup = ProviderConfigInput {
            index: 1,
            id: "GOOGLE",
            name: "Google",
            display_name: "",
            issuer: "",
            issuer_url: "https://g2",
            client_id: "c2",
            auth_url: "",
            token_url: "",
            redirect_url: "",
            extra_scopes: &[],
        };
        match validate_oidc_provider_config(&dup, &mut seen) {
            Err(ProviderConfigError::DuplicateProviderId {
                id,
                previous,
                index,
            }) => {
                assert_eq!(id, "GOOGLE");
                assert_eq!(previous, "Google");
                assert_eq!(index, 1);
            }
            other => panic!("expected DuplicateProviderId, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_missing_endpoints() {
        // biz/oidc.go:261-264: discovery failed AND no manual endpoints.
        let input = ProviderConfigInput {
            index: 0,
            id: "p",
            name: "p",
            display_name: "",
            issuer: "",
            issuer_url: "", // no discovery URL
            client_id: "c1",
            auth_url: "", // and no manual endpoints
            token_url: "",
            redirect_url: "",
            extra_scopes: &[],
        };
        let mut seen = BTreeMap::new();
        match validate_oidc_provider_config(&input, &mut seen) {
            Err(ProviderConfigError::MissingRequiredField { id, field }) => {
                assert_eq!(id, "p");
                assert_eq!(field, "issuer_url or (auth_url and token_url)");
            }
            other => panic!("expected MissingRequiredField(endpoints), got {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_manual_endpoints_when_no_discovery_url() -> Result<(), String> {
        // biz/oidc.go:243-248: AuthURL + TokenURL override discovery.
        let input = ProviderConfigInput {
            index: 0,
            id: "manual",
            name: "manual",
            display_name: "",
            issuer: "https://manual.example",
            issuer_url: "", // discovery not configured
            client_id: "c1",
            auth_url: "https://manual.example/oauth2/auth",
            token_url: "https://manual.example/oauth2/token",
            redirect_url: "",
            extra_scopes: &[],
        };
        let mut seen = BTreeMap::new();
        let (id, scopes) = match validate_oidc_provider_config(&input, &mut seen) {
            Ok(v) => v,
            Err(e) => return Err(format!("manual endpoints should be accepted: {e:?}")),
        };
        assert_eq!(id, "manual");
        // biz/oidc.go:285-293: empty ExtraScopes -> defaults.
        assert_eq!(
            scopes,
            vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ]
        );
        Ok(())
    }

    #[test]
    fn validate_rejects_missing_client_id() {
        // Surfaced for parity clarity; Go silently wires an unusable config
        // but we want the pure helper to be total.
        let mut input = provider_input("p", "https://idp.example", "");
        input.id = "p";
        let mut seen = BTreeMap::new();
        match validate_oidc_provider_config(&input, &mut seen) {
            Err(ProviderConfigError::MissingRequiredField { id, field }) => {
                assert_eq!(id, "p");
                assert_eq!(field, "client_id");
            }
            other => panic!("expected MissingRequiredField(client_id), got {other:?}"),
        }
    }

    #[test]
    fn validate_returns_extra_scopes_when_provided() -> Result<(), String> {
        // biz/oidc.go:285-286: "scopes = p.ExtraScopes" when non-empty.
        let scopes = vec!["openid".to_string(), "custom_scope".to_string()];
        let mut input = provider_input("p", "https://idp.example", "c1");
        input.extra_scopes = &scopes;
        let mut seen = BTreeMap::new();
        let (id, returned_scopes) = match validate_oidc_provider_config(&input, &mut seen) {
            Ok(v) => v,
            Err(e) => return Err(format!("valid config should pass: {e:?}")),
        };
        assert_eq!(id, "p");
        assert_eq!(returned_scopes, scopes);
        Ok(())
    }

    // --- build_callback_redirect_url (biz/oidc.go:277-283) -----------------

    #[test]
    fn redirect_url_explicit_value_wins() {
        // biz/oidc.go:277-278: p.RedirectURL wins when non-empty.
        let url = build_callback_redirect_url("/custom/cb", "p", 5);
        assert_eq!(url, "/custom/cb");
    }

    #[test]
    fn redirect_url_single_provider_uses_bare_callback() {
        // biz/oidc.go:278-280: numProviders == 1 -> "/oauth/oidc/callback".
        let url = build_callback_redirect_url("", "p", 1);
        assert_eq!(url, "/oauth/oidc/callback");
    }

    #[test]
    fn redirect_url_multi_provider_appends_provider_id() {
        // biz/oidc.go:280-282: numProviders > 1 -> "/oauth/oidc/callback/{id}".
        let url = build_callback_redirect_url("", "google", 3);
        assert_eq!(url, "/oauth/oidc/callback/google");
    }

    // --- extract_callback_params (api/oidc.go:144-156) ---------------------

    fn query_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn extract_returns_code_and_state_when_present() {
        // api/oidc.go:144-145: happy path.
        let pairs = query_pairs(&[("code", "abc"), ("state", "xyz")]);
        let params = match extract_callback_params(pairs) {
            Ok(p) => p,
            Err(e) => panic!("happy path should pass: {e:?}"),
        };
        assert_eq!(params.code, "abc");
        assert_eq!(params.state, "xyz");
        assert_eq!(params.error, "");
        assert_eq!(params.error_description, "");
    }

    #[test]
    fn extract_short_circuits_on_provider_error() {
        // api/oidc.go:148-151: error present -> immediately return error_description.
        let pairs = query_pairs(&[
            ("error", "access_denied"),
            ("error_description", "user cancelled"),
            ("code", "ignored"),
            ("state", "ignored"),
        ]);
        match extract_callback_params(pairs) {
            Err(CallbackExtractError::ProviderError(msg)) => {
                assert_eq!(msg, "user cancelled");
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn extract_falls_back_to_error_code_when_description_missing() {
        // Mirror Go response body `{"error": c.Query("error_description")}`:
        // when error_description is empty the user-visible message falls back
        // to the error short code.
        let pairs = query_pairs(&[("error", "access_denied"), ("code", "x")]);
        match extract_callback_params(pairs) {
            Err(CallbackExtractError::ProviderError(msg)) => {
                assert_eq!(msg, "access_denied");
            }
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_missing_code() {
        // api/oidc.go:153-156: "Code and state are required".
        let pairs = query_pairs(&[("state", "xyz")]);
        match extract_callback_params(pairs) {
            Err(CallbackExtractError::MissingCodeOrState) => {}
            other => panic!("expected MissingCodeOrState, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_missing_state() {
        let pairs = query_pairs(&[("code", "abc")]);
        match extract_callback_params(pairs) {
            Err(CallbackExtractError::MissingCodeOrState) => {}
            other => panic!("expected MissingCodeOrState, got {other:?}"),
        }
    }

    #[test]
    fn extract_rejects_empty_query() {
        let pairs: Vec<(String, String)> = Vec::new();
        match extract_callback_params(pairs) {
            Err(CallbackExtractError::MissingCodeOrState) => {}
            other => panic!("expected MissingCodeOrState, got {other:?}"),
        }
    }

    // --- callback_intent (biz/oidc.go:601-614, 737-753) --------------------

    #[test]
    fn intent_login_round_trips_verbatim() {
        // biz/oidc.go:775: no link-state cache entry -> "login".
        let token = encode_callback_intent("state-abc", CallbackIntent::Login);
        assert_eq!(token, "state-abc");
        assert_eq!(callback_intent(&token), CallbackIntent::Login);
    }

    #[test]
    fn intent_link_round_trips_with_prefix() {
        // biz/oidc.go:752: link-state cache entry present -> "link".
        let token = encode_callback_intent("state-xyz", CallbackIntent::Link);
        assert_eq!(token, format!("{LINK_STATE_PREFIX}state-xyz"));
        assert_eq!(callback_intent(&token), CallbackIntent::Link);
    }

    #[test]
    fn encode_link_intent_is_idempotent() {
        // Calling encode twice should not stack prefixes.
        let once = encode_callback_intent("state-xyz", CallbackIntent::Link);
        let twice = encode_callback_intent(&once, CallbackIntent::Link);
        assert_eq!(once, twice);
        assert_eq!(callback_intent(&once), CallbackIntent::Link);
    }

    #[test]
    fn intent_login_for_bare_token() {
        // Any token without the link prefix is a login flow.
        assert_eq!(callback_intent("plainstate"), CallbackIntent::Login);
        assert_eq!(callback_intent(""), CallbackIntent::Login);
    }

    #[test]
    fn intent_link_only_when_prefix_present() {
        assert_eq!(
            callback_intent(&format!("{LINK_STATE_PREFIX}tok")),
            CallbackIntent::Link
        );
        // Token that merely contains the prefix elsewhere is NOT link.
        assert_eq!(
            callback_intent(&format!("tok{LINK_STATE_PREFIX}")),
            CallbackIntent::Login
        );
    }

    // --- verify_pkce (RFC 7636 Appendix B; biz/oidc.go:592) ----------------

    #[test]
    fn verify_pkce_accepts_rfc_7636_vector() {
        // RFC 7636 Appendix B vector — same one used by
        // conduit-http::oidc_helpers::pkce_challenge test.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(verify_pkce(verifier, challenge));
    }

    #[test]
    fn verify_pkce_rejects_wrong_verifier() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(!verify_pkce("wrong-verifier", challenge));
    }

    #[test]
    fn verify_pkce_rejects_wrong_challenge() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(!verify_pkce(
            verifier,
            "totally-different-challenge-value-xxx"
        ));
    }

    #[test]
    fn verify_pkce_rejects_length_mismatch() {
        // Different lengths should not cause a panic and must return false.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(!verify_pkce(verifier, "short"));
        assert!(!verify_pkce(
            "short",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }

    #[test]
    fn verify_pkce_matches_generated_challenge() {
        // verify_pkce must accept the challenge produced by
        // PkceChallenge::generate() / pkce_challenge_from_verifier().
        let pkce = PkceChallenge::generate();
        assert!(verify_pkce(&pkce.verifier, &pkce.challenge));
        assert_eq!(pkce.challenge, pkce_challenge_from_verifier(&pkce.verifier));
    }

    #[test]
    fn pkce_challenge_from_verifier_matches_rfc_vector() {
        // Same vector as conduit-http::oidc_helpers::pkce_challenge test —
        // proves the local helper produces byte-identical output without
        // depending on conduit-http.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            pkce_challenge_from_verifier(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
}

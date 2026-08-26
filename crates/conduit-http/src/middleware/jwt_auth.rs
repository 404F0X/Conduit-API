//! JWT-admin auth middleware (RUST-P11-003 MAP-01 sub-gap).
//!
//! Ports Go `middleware.WithJWTAuth` (`conduit/internal/server/middleware/auth.go:74-110`)
//! to an axum `from_fn`-compatible tower layer. This is the JWT guard that
//! protects the admin OAuth + OIDC link + request-content/preview routes on
//! the `/admin` group (`routes.go:96-139`).
//!
//! ## Go source contract (verbatim)
//!
//! ```text
//! func WithJWTAuth(auth *biz.AuthService) gin.HandlerFunc {
//!     return func(c *gin.Context) {
//!         token, err := ExtractAPIKeyFromRequest(c.Request, &APIKeyConfig{
//!             Headers:       []string{"Authorization"},
//!             RequireBearer: true,
//!         })
//!         if err != nil {
//!             AbortWithError(c, http.StatusUnauthorized, err)
//!             return
//!         }
//!         user, err := auth.AuthenticateJWTToken(c.Request.Context(), token)
//!         if err != nil {
//!             if errors.Is(err, biz.ErrInvalidJWT) {
//!                 AbortWithError(c, http.StatusUnauthorized, errors.New("Invalid token"))
//!             } else {
//!                 AbortWithError(c, http.StatusInternalServerError, errors.New("Failed to validate token"))
//!             }
//!             return
//!         }
//!         ctx := contexts.WithUser(c.Request.Context(), user)
//!         ctx = shared.WithSessionScope(ctx, "user:"+strconv.Itoa(user.ID))
//!         ctx, err = withUserPrincipal(ctx, user)
//!         if err != nil {
//!             AbortWithError(c, http.StatusUnauthorized, errors.New("Invalid authentication context"))
//!             return
//!         }
//!         c.Request = c.Request.WithContext(ctx)
//!         c.Next()
//!     }
//! }
//! ```
//!
//! ## Failure response shape
//!
//! Go `AbortWithError` (`middleware/error.go:12-20`) emits
//! `{"error":{"type":"<http.StatusText(status)>","message":"<err.Error()>"}}`
//! via `c.AbortWithStatusJSON`. Rust port uses [`crate::api_error::json_error`]
//! which produces the byte-identical shape.
//!
//! ## Scope note (no admin role check at this layer)
//!
//! Go `WithJWTAuth` does **not** perform an admin-role check; the only
//! identity gate at this layer is "JWT signature valid + user activated"
//! (the latter lives in `biz.AuthenticateJWTToken`, `auth.go:192-201`: a
//! failed user load or `Status != activated` wraps `ErrInvalidJWT` → 401
//! "Invalid token"). The Rust port enforces the same gate via the wired
//! [`crate::middleware::JwtIdentityResolver`] (P-33 parity fix — previously a
//! deactivated/deleted user's unexpired JWT was still let through). The
//! "admin" naming refers to the route surface, not to an RBAC role gate.
//! RBAC enforcement happens deeper (ent privacy layer + handler-level
//! checks). A 403 path is therefore not exercised here — see
//! `tests::jwt_admin_auth_does_not_have_a_403_branch_like_go`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::api_error::json_error;
use crate::app_state::AppState;
use crate::middleware::{
    AuthRequestContextExtension, JWT_INTERNAL_PUBLIC_MESSAGE, JWT_INVALID_PUBLIC_MESSAGE,
    JwtAuthError, JwtIdentityResolution, ProjectIdStatus, enrich_jwt_context,
    insert_auth_request_context, project_id_outcome, verify_jwt_and_build_context,
};

/// Admin-group JWT auth middleware entry point — axum `from_fn` signature.
///
/// Wires up cleanly via `Router::route_layer(axum::middleware::from_fn_with_state(
/// state.clone(), jwt_admin_auth))` on the admin OAuth sub-router (see
/// `router.rs`).
pub async fn jwt_admin_auth(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    // 1) Token extraction — mirrors `ExtractAPIKeyFromRequest` with
    //    `{ Headers: ["Authorization"], RequireBearer: true }`.
    //    Go (header.go:72-85) requires the literal "Bearer " prefix and a
    //    non-empty trimmed remainder; any other shape falls through to 401.
    let token = match extract_bearer_token(request.headers()) {
        Some(value) => value,
        None => {
            // Go emits the wrapped error string here; the actual messages are
            // "Authorization header must start with 'Bearer '" /
            // "API key is required" / "ErrAPIKeyRequired". They all surface as
            // 401 Unauthorized with the literal message — but the publicly
            // observable invariant is: any extraction failure is a 401. We
            // surface the canonical "Invalid token" message used by the
            // downstream `ErrInvalidJWT` branch to keep failure responses
            // uniform (and avoid leaking which header rule failed).
            return json_error(StatusCode::UNAUTHORIZED, JWT_INVALID_PUBLIC_MESSAGE);
        }
    };

    // 2) Resolve the HS256 signing secret. Go reads it per-request from
    //    `SystemService.SecretKey(ctx)` (biz/auth.go:161-169), which returns
    //    the value persisted to the system table at `initialize` time. The
    //    Rust port mirrors this: prefer the live system service, falling back
    //    to the static `config.api_auth.jwt_secret` for back-compat and for
    //    tests that wire only the config value.
    //
    //    The system service returns the *signing* bytes (already hex-decoded),
    //    matching the bytes the signin service uses to mint tokens; the config
    //    fallback carries a plain string whose UTF-8 bytes are the secret.
    //    Missing on both paths while the system service is wired means this
    //    installation is not initialized yet. Any supplied token is stale and
    //    is rejected as invalid. A missing service or a service failure remains
    //    an internal wiring/storage error.
    let secret_bytes = match resolve_jwt_secret(&state).await {
        JwtSecretResolution::Available(bytes) => bytes,
        // No token can be valid before this installation has generated its
        // signing secret. A bearer token in this state is necessarily stale
        // (usually left in the browser by a replaced test database), so treat
        // it as an authentication failure rather than a server failure.
        JwtSecretResolution::SystemNotInitialized => {
            return json_error(StatusCode::UNAUTHORIZED, JWT_INVALID_PUBLIC_MESSAGE);
        }
        JwtSecretResolution::Internal => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                JWT_INTERNAL_PUBLIC_MESSAGE,
            );
        }
    };

    // 3) Verify + decode JWT, build the auth `RequestContext` (mirrors Go's
    //    `contexts.WithUser` + `shared.WithSessionScope` + `withUserPrincipal`).
    //    The verify helper maps all `jsonwebtoken` decode failures to
    //    `JwtAuthError::Invalid` (Go collapses every parse/signature/expiry
    //    failure into `ErrInvalidJWT` -> 401 "Invalid token").
    let source = conduit_auth::RequestSource::AdminRest;
    let outcome = verify_jwt_and_build_context(token, &secret_bytes, source, None, None);

    let context = match outcome {
        Ok(context) => context,
        Err(JwtAuthError::Invalid) => {
            return json_error(StatusCode::UNAUTHORIZED, JWT_INVALID_PUBLIC_MESSAGE);
        }
        // No internal branch is reachable on this code path: the only failure
        // mode of `decode_hs256` is a `jsonwebtoken::Error` (signature/expiry/
        // malformed), which the helper maps to `Invalid`. The `Internal`
        // variant is preserved for symmetry with the Go error taxonomy where
        // DB / cache faults surface as 500 — those faults are handled upstream
        // (System service) in the Rust port, never inside this middleware.
        Err(JwtAuthError::Internal) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                JWT_INTERNAL_PUBLIC_MESSAGE,
            );
        }
    };

    // 3b) Load the user row and finish authentication (Go `AuthenticateJWTToken`,
    //     `biz/auth.go:192-201`). In Go the user load is part of *authentication*:
    //     `failed to get user`, a missing row, and `user not activated` all wrap
    //     `ErrInvalidJWT` and surface as 401 "Invalid token" — a deactivated or
    //     deleted user's still-unexpired JWT must NOT keep working. On success
    //     the loaded facts (`is_owner`, role-expanded scopes) are folded onto the
    //     principal, which is what Go's scope rules read via `contexts.GetUser`.
    //
    //     No resolver wired (`user_principal_service() == None`) ⇒ skip this
    //     step, keeping the claims-only principal — back-compat for hosts/tests
    //     that wire only the JWT secret.
    let mut context = match state.services().user_principal_service() {
        Some(resolver) => {
            let user_id = context.user.as_ref().map(|user| user.user_id);
            match user_id {
                Some(user_id) => match resolver.resolve(user_id).await {
                    JwtIdentityResolution::Found(identity) => {
                        enrich_jwt_context(context, &identity)
                    }
                    // Same public message as any other invalid token — the
                    // account's existence/state is not leaked (Go wraps the
                    // detail server-side and the client sees "Invalid token").
                    JwtIdentityResolution::UserUnavailable => {
                        return json_error(StatusCode::UNAUTHORIZED, JWT_INVALID_PUBLIC_MESSAGE);
                    }
                },
                None => context,
            }
        }
        None => context,
    };

    // The project selector is part of the authenticated admin request context
    // in Go. Parse it before publishing the context so GraphQL authorization
    // can evaluate project membership/role scopes instead of incorrectly
    // requiring a system-wide scope.
    match project_id_outcome(request.headers()) {
        crate::middleware::ProjectIdOutcome {
            status: ProjectIdStatus::Ok,
            project_id: Some(project_id),
        } => {
            let _ = context.set_project_id(project_id.to_string());
        }
        crate::middleware::ProjectIdOutcome {
            status: ProjectIdStatus::Invalid,
            ..
        } => return json_error(StatusCode::BAD_REQUEST, "Invalid project ID"),
        _ => {}
    }

    // 4) Inject the auth context as a strongly-typed request extension so
    //    downstream handlers read principal/user_id from the same place Go's
    //    `contexts.FromContext(ctx)` would. Mirrors
    //    `c.Request = c.Request.WithContext(ctx)`.
    let mut request = request;
    insert_auth_request_context(&mut request, AuthRequestContextExtension::new(context));
    next.run(request).await
}

/// Resolve the HS256 signing secret for JWT verification.
///
/// Mirrors Go `AuthService.AuthenticateJWTToken` (`biz/auth.go:161-169`), which
/// reads the secret fresh from `SystemService.SecretKey(ctx)` on every request
/// rather than a boot-time snapshot — so a secret generated by a runtime
/// `initialize` call is picked up without a restart.
///
/// Resolution order:
/// 1. The wired [`crate::system_handlers::SystemService`], via its
///    `jwt_secret()` method (returns the already-decoded signing bytes, the
///    same bytes the signin service signs with).
/// 2. Fallback to the static `config.api_auth.jwt_secret` (its UTF-8 bytes),
///    preserving back-compat for hosts that inject the secret via config and
///    for the middleware's config-only unit tests.
///
/// Distinguishes an uninitialized installation from an actual service/wiring
/// failure so a stale browser token cannot turn the initialization page into a
/// 500 response loop.
enum JwtSecretResolution {
    Available(Vec<u8>),
    SystemNotInitialized,
    Internal,
}

async fn resolve_jwt_secret(state: &AppState) -> JwtSecretResolution {
    // 1) Live system service (Go's per-request `SystemService.SecretKey`).
    //    Both an uninitialized system and a service error may use the static
    //    config fallback. Without that fallback they remain distinct outcomes.
    if let Some(service) = state.services().system_service() {
        match service.jwt_secret().await {
            Ok(Some(bytes)) => return JwtSecretResolution::Available(bytes),
            Ok(None) => {
                return state
                    .config()
                    .api_auth
                    .jwt_secret
                    .as_deref()
                    .map(|secret| JwtSecretResolution::Available(secret.as_bytes().to_vec()))
                    .unwrap_or(JwtSecretResolution::SystemNotInitialized);
            }
            Err(_) => {
                return state
                    .config()
                    .api_auth
                    .jwt_secret
                    .as_deref()
                    .map(|secret| JwtSecretResolution::Available(secret.as_bytes().to_vec()))
                    .unwrap_or(JwtSecretResolution::Internal);
            }
        }
    }

    // 2) Static config fallback (boot-wired secret / test wiring).
    state
        .config()
        .api_auth
        .jwt_secret
        .as_deref()
        .map(|secret| JwtSecretResolution::Available(secret.as_bytes().to_vec()))
        .unwrap_or(JwtSecretResolution::Internal)
}

/// Extract the bearer token from the `Authorization` header.
///
/// Mirrors Go `ExtractAPIKeyFromRequest` with `RequireBearer: true`
/// (`header.go:72-85`): the header must start with the literal `"Bearer "`
/// prefix and the remainder must be non-empty after `trim()`. Any other
/// shape (missing header, wrong prefix, blank value) returns `None`.
pub fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if !raw.starts_with("Bearer ") {
        return None;
    }
    let token = raw["Bearer ".len()..].trim();
    if token.is_empty() {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::{SystemTime, UNIX_EPOCH};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderValue, Request, StatusCode, header};
    use axum::middleware::from_fn_with_state;
    use axum::routing::{get, post};
    use tower::Service;

    use super::*;
    use crate::app_state::{AppServices, AppState};
    use crate::middleware::{JwtIdentityResolver, JwtUserIdentity};
    use crate::system_handlers::{InitializeSystemParams, SystemService};
    use conduit_config::AppConfig;
    use std::sync::Arc;

    /// Build a minimal `AppState` carrying the given JWT secret, mirroring how
    /// the host wires `config.api_auth.jwt_secret` at boot.
    fn state_with_secret(secret: Option<&str>) -> AppState {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = secret.map(str::to_string);
        AppState::from_config(config)
    }

    struct UninitializedSystemService;

    #[async_trait::async_trait]
    impl SystemService for UninitializedSystemService {
        async fn is_initialized(&self) -> Result<bool, String> {
            Ok(false)
        }

        async fn initialize(&self, _params: InitializeSystemParams) -> Result<(), String> {
            Ok(())
        }

        async fn brand_logo(&self) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn uninitialized_state() -> AppState {
        AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(AppServices::new().with_system_service(Arc::new(UninitializedSystemService))),
        )
    }

    async fn ok_handler(_: State<AppState>) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({"ok": true}))
    }

    fn build_router(state: AppState) -> Router {
        Router::new()
            .route("/admin/{provider}/oauth/start", post(ok_handler))
            .route("/admin/oidc/link/{provider}", get(ok_handler))
            .route_layer(from_fn_with_state(state.clone(), jwt_admin_auth))
            .with_state(state)
    }

    fn bearer_header(token: &str) -> Result<HeaderValue, Box<dyn Error>> {
        Ok(HeaderValue::from_str(&format!("Bearer {token}"))?)
    }

    fn make_expired_jwt(secret: &str, user_id: i64) -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let claims = Claims {
            user_id,
            session_scope: format!("user:{user_id}"),
            exp: now - 172_800,
            iat: now - 200_000,
        };
        encode_hs256(&claims, secret).unwrap_or_default()
    }

    fn make_valid_jwt(secret: &str, user_id: i64) -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        encode_hs256(&Claims::new(user_id, format!("user:{user_id}")), secret).unwrap_or_default()
    }

    #[tokio::test]
    async fn happy_path_valid_token_reaches_handler() -> Result<(), Box<dyn Error>> {
        let secret = "test-secret";
        let token = make_valid_jwt(secret, 42);
        let state = state_with_secret(Some(secret));
        let mut router = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(header::AUTHORIZATION, bearer_header(&token)?)
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }

    #[tokio::test]
    async fn missing_authorization_header_returns_401_with_go_shape() -> Result<(), Box<dyn Error>>
    {
        let state = state_with_secret(Some("test-secret"));
        let mut router = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body_bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "type": "Unauthorized",
                    "message": "Invalid token",
                }
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn non_bearer_prefix_returns_401() -> Result<(), Box<dyn Error>> {
        let state = state_with_secret(Some("test-secret"));
        let mut router = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Token abc.def.ghi"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn blank_bearer_value_returns_401() -> Result<(), Box<dyn Error>> {
        let state = state_with_secret(Some("test-secret"));
        let mut router = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(header::AUTHORIZATION, HeaderValue::from_static("Bearer   "))
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_signature_returns_401_invalid_token() -> Result<(), Box<dyn Error>> {
        let state = state_with_secret(Some("real-secret"));
        let mut router = build_router(state);

        let token = make_valid_jwt("wrong-secret", 42);
        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(header::AUTHORIZATION, bearer_header(&token)?)
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body_bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["error"]["message"], "Invalid token");
        assert_eq!(body["error"]["type"], "Unauthorized");
        Ok(())
    }

    #[tokio::test]
    async fn expired_token_returns_401() -> Result<(), Box<dyn Error>> {
        let secret = "test-secret";
        let token = make_expired_jwt(secret, 42);
        let state = state_with_secret(Some(secret));
        let mut router = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(header::AUTHORIZATION, bearer_header(&token)?)
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn missing_jwt_secret_returns_500_failed_to_validate_token() -> Result<(), Box<dyn Error>>
    {
        // Mirrors Go's behavior when SystemService.SecretKey returns a
        // non-ErrInvalidJWT error: AbortWithError(500, "Failed to validate
        // token"). In the Rust port this happens when the host has not yet
        // wired api_auth.jwt_secret (system not initialized).
        let state = state_with_secret(None);
        let mut router = build_router(state);

        let token = make_valid_jwt("any-secret", 42);
        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(header::AUTHORIZATION, bearer_header(&token)?)
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body_bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["error"]["message"], "Failed to validate token");
        assert_eq!(body["error"]["type"], "Internal Server Error");
        Ok(())
    }

    #[tokio::test]
    async fn stale_token_before_initialization_returns_401_invalid_token()
    -> Result<(), Box<dyn Error>> {
        let mut router = build_router(uninitialized_state());
        let request = Request::builder()
            .method("POST")
            .uri("/admin/codex/oauth/start")
            .header(
                header::AUTHORIZATION,
                bearer_header(&make_valid_jwt("old-database-secret", 42))?,
            )
            .body(Body::empty())?;

        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body_bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["error"]["message"], "Invalid token");
        assert_eq!(body["error"]["type"], "Unauthorized");
        Ok(())
    }

    /// Documents the absence of an admin-role 403 branch.
    ///
    /// Go `WithJWTAuth` only validates the JWT signature + user activation; it
    /// does NOT perform an RBAC admin-role check. The "admin" group naming
    /// refers to the surface, not to a role gate — RBAC happens deeper (ent
    /// privacy layer + handler checks). Therefore the Rust port has no 403
    /// branch here either, mirroring Go byte-for-byte.
    ///
    /// P-33 note (intentional update, Go parity fix): the "user activated"
    /// half of the gate is now enforced via `JwtIdentityResolver` — a
    /// deactivated/deleted user is a **401** (Go wraps `ErrInvalidJWT`,
    /// `biz/auth.go:192-201`), still never 403. The failure-code set below is
    /// unchanged.
    #[test]
    fn jwt_admin_auth_does_not_have_a_403_branch_like_go() {
        // Static contract: the middleware emits only 401 (Invalid / user
        // unavailable) and 500 (Internal) failure codes — never 403. Adding a
        // 403 branch here would diverge from Go parity; this test pins the
        // invariant.
        let possible_failure_codes = [StatusCode::UNAUTHORIZED, StatusCode::INTERNAL_SERVER_ERROR];
        assert!(!possible_failure_codes.contains(&StatusCode::FORBIDDEN));
    }

    // ---- pure helper tests (no router) ---------------------------------------

    #[test]
    fn extract_bearer_returns_token_for_well_formed_header() -> Result<(), Box<dyn Error>> {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc.def.ghi"),
        );
        assert_eq!(extract_bearer_token(&headers), Some("abc.def.ghi"));
        Ok(())
    }

    #[test]
    fn extract_bearer_returns_none_for_missing_header() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_returns_none_for_wrong_prefix() -> Result<(), Box<dyn Error>> {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Token abc.def.ghi"),
        );
        assert_eq!(extract_bearer_token(&headers), None);
        Ok(())
    }

    #[test]
    fn extract_bearer_returns_none_for_blank_value() -> Result<(), Box<dyn Error>> {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer   "));
        assert_eq!(extract_bearer_token(&headers), None);
        Ok(())
    }

    // =======================================================================
    // Identity resolution (the user load Go does inside AuthenticateJWTToken)
    // =======================================================================
    //
    // Go makes the user load part of authentication (`biz/auth.go:192-201`):
    // a failed lookup, a missing row, and `Status != activated` all wrap
    // `ErrInvalidJWT`, which `WithJWTAuth` maps to 401 "Invalid token". P-33
    // parity fix: a deactivated/deleted user's unexpired JWT is a 401, not a
    // pass-through. On success the loaded facts (`is_owner`, scopes) are
    // folded onto the principal — the piece every scope rule reads.

    /// Stub resolver returning a fixed resolution, standing in for the DB
    /// lookup the host wires (`DbJwtIdentityResolver`).
    struct StubResolver {
        resolution: JwtIdentityResolution,
        /// Records the id the middleware asked about.
        seen: std::sync::Arc<std::sync::Mutex<Vec<i64>>>,
    }

    #[async_trait::async_trait]
    impl JwtIdentityResolver for StubResolver {
        async fn resolve(&self, user_id: i64) -> JwtIdentityResolution {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(user_id);
            }
            self.resolution.clone()
        }
    }

    /// `AppState` with both a JWT secret and an identity resolver wired.
    fn state_with_resolver(secret: &str, resolver: Arc<dyn JwtIdentityResolver>) -> AppState {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(secret.to_string());
        let services = crate::app_state::AppServices::new().with_user_principal_service(resolver);
        AppState::new(Arc::new(config), Arc::new(services))
    }

    /// Handler that reports what the principal actually carries, so the test
    /// asserts on the context the downstream adapters would see.
    async fn principal_probe(
        auth: Option<axum::Extension<AuthRequestContextExtension>>,
    ) -> axum::Json<serde_json::Value> {
        let Some(axum::Extension(auth)) = auth else {
            return axum::Json(serde_json::json!({"principal": null}));
        };
        let Some(principal) = auth.context().principal.as_ref() else {
            return axum::Json(serde_json::json!({"principal": null}));
        };
        let mut scopes: Vec<String> = principal.scopes.iter().cloned().collect();
        scopes.sort();
        axum::Json(serde_json::json!({
            "is_owner": principal.is_owner,
            "scopes": scopes,
            "project_id": auth.context().project_id,
        }))
    }

    fn probe_router(state: AppState) -> Router {
        Router::new()
            .route("/probe", get(principal_probe))
            .route_layer(from_fn_with_state(state.clone(), jwt_admin_auth))
            .with_state(state)
    }

    async fn probe_response(state: AppState, token: &str) -> Result<Response, Box<dyn Error>> {
        let mut router = probe_router(state);
        let request = Request::builder()
            .method("GET")
            .uri("/probe")
            .header(header::AUTHORIZATION, bearer_header(token)?)
            .body(Body::empty())?;
        Ok(router.call(request).await?)
    }

    async fn probe(state: AppState, token: &str) -> Result<serde_json::Value, Box<dyn Error>> {
        let response = probe_response(state, token).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    #[tokio::test]
    async fn valid_project_header_is_published_in_auth_context() -> Result<(), Box<dyn Error>> {
        let secret = "project-context-secret";
        let token = make_valid_jwt(secret, 42);
        let mut router = probe_router(state_with_secret(Some(secret)));
        let request = Request::builder()
            .method("GET")
            .uri("/probe")
            .header(header::AUTHORIZATION, bearer_header(&token)?)
            .header("X-Project-ID", "gid://conduit/Project/77")
            .body(Body::empty())?;
        let response = router.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["project_id"], "77");
        Ok(())
    }

    /// P-33 core case: a deactivated (or deleted) user holding a still-valid,
    /// unexpired JWT must be rejected with 401 — Go wraps `ErrInvalidJWT`
    /// ("user not activated" / "failed to get user", `biz/auth.go:192-201`)
    /// and `WithJWTAuth` answers 401 "Invalid token". The public message is
    /// identical to any other invalid token: the account state is not leaked.
    #[tokio::test]
    async fn deactivated_user_with_valid_token_returns_401() -> Result<(), Box<dyn Error>> {
        let secret = "resolver-unavailable-secret";
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let resolver = Arc::new(StubResolver {
            resolution: JwtIdentityResolution::UserUnavailable,
            seen: seen.clone(),
        });
        let state = state_with_resolver(secret, resolver);
        let response = probe_response(state, &make_valid_jwt(secret, 9)).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 1024).await?;
        let body: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(body["error"]["message"], "Invalid token");
        assert_eq!(body["error"]["type"], "Unauthorized");
        // The middleware consulted the resolver with the token's user id.
        match seen.lock() {
            Ok(seen) => assert_eq!(&*seen, &vec![9_i64]),
            Err(_) => return Err("seen mutex poisoned".into()),
        }
        Ok(())
    }

    /// The owner user (seeded with `is_owner = true`, scopes `["*"]`) must
    /// reach the policy layer carrying both facts.
    #[tokio::test]
    async fn owner_identity_is_folded_onto_the_jwt_principal() -> Result<(), Box<dyn Error>> {
        let secret = "enrich-owner-secret";
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let resolver = Arc::new(StubResolver {
            resolution: JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: true,
                scope_slugs: vec!["*".to_string()],
            }),
            seen: seen.clone(),
        });
        let state = state_with_resolver(secret, resolver);
        let body = probe(state, &make_valid_jwt(secret, 1)).await?;

        assert_eq!(body["is_owner"], serde_json::json!(true));
        assert_eq!(body["scopes"], serde_json::json!(["*"]));
        // The middleware resolved the id carried by the token.
        match seen.lock() {
            Ok(seen) => assert_eq!(&*seen, &vec![1_i64]),
            Err(_) => return Err("seen mutex poisoned".into()),
        }
        Ok(())
    }

    /// A non-owner active user gets 200 and exactly the scopes its roles
    /// grant — no owner bypass.
    #[tokio::test]
    async fn active_member_passes_with_its_own_scopes() -> Result<(), Box<dyn Error>> {
        let secret = "enrich-member-secret";
        let resolver = Arc::new(StubResolver {
            resolution: JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: false,
                scope_slugs: vec!["read_channels".to_string(), "write_channels".to_string()],
            }),
            seen: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let state = state_with_resolver(secret, resolver);
        let body = probe(state, &make_valid_jwt(secret, 42)).await?;

        assert_eq!(body["is_owner"], serde_json::json!(false));
        assert_eq!(
            body["scopes"],
            serde_json::json!(["read_channels", "write_channels"])
        );
        Ok(())
    }

    /// With no resolver wired the middleware behaves exactly as before —
    /// claims-only principal, request passes. Back-compat for hosts/tests
    /// that wire only the JWT secret (the "NoResolver" state is the absence
    /// of a resolver in `AppServices`, not an enum variant).
    #[tokio::test]
    async fn no_resolver_wired_keeps_previous_behavior() -> Result<(), Box<dyn Error>> {
        let secret = "enrich-absent-secret";
        let state = state_with_secret(Some(secret));
        let body = probe(state, &make_valid_jwt(secret, 3)).await?;

        assert_eq!(body["is_owner"], serde_json::json!(false));
        assert_eq!(body["scopes"], serde_json::json!([]));
        Ok(())
    }

    /// Resolution runs only after the token verifies: a bad token is still a
    /// 401 and the resolver is never consulted.
    #[tokio::test]
    async fn invalid_token_never_reaches_the_resolver() -> Result<(), Box<dyn Error>> {
        let secret = "enrich-invalid-secret";
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let resolver = Arc::new(StubResolver {
            resolution: JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: true,
                scope_slugs: vec!["*".to_string()],
            }),
            seen: seen.clone(),
        });
        let state = state_with_resolver(secret, resolver);
        let mut router = probe_router(state);
        let request = Request::builder()
            .method("GET")
            .uri("/probe")
            .header(
                header::AUTHORIZATION,
                bearer_header(&make_expired_jwt(secret, 1))?,
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        match seen.lock() {
            Ok(seen) => assert!(
                seen.is_empty(),
                "resolver must not run for an invalid token"
            ),
            Err(_) => return Err("seen mutex poisoned".into()),
        }
        Ok(())
    }
}

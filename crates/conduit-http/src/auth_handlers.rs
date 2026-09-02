//! Auth REST handler: `POST /admin/auth/signin` (RUST-P11-003 S10 — frontend
//! smoke entry).
//!
//! Ports the gin handler body of `conduit/internal/server/api/auth.go`
//! (`AuthHandlers.SignIn`, auth.go:43-81). The pure `SignInResponse` shaping
//! (S06) already lives in [`crate::admin_handlers`]; this module adds the axum
//! handler plus the minimal [`SigninService`] trait the host wires at boot
//! (Go injects `*biz.AuthService` via fx — auth.go:14-28).

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conduit_core::objects::user::UserInfo;
use serde::Deserialize;

use crate::admin_handlers::build_signin_response;
use crate::api_error::{is_valid_email, json_error, min_chars};
use crate::app_state::AppState;

/// `SignInRequest` — ported 1:1 from Go (`api/auth.go:31-34`):
///
/// ```text
/// Email    string `json:"email"    binding:"required,email"`
/// Password string `json:"password" binding:"required"`
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct SignInRequest {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
}

/// Public email/password registration request. Names are optional so the
/// retained frontend's compact form can register with only email/password.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SignUpRequest {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub first_name: String,
    #[serde(default)]
    pub last_name: String,
}

impl SignUpRequest {
    pub fn passes_binding(&self) -> bool {
        !self.email.is_empty() && is_valid_email(&self.email) && min_chars(&self.password, 8)
    }
}

impl SignInRequest {
    /// Enforce the gin binding tags (auth.go:32-33). Any failure collapses
    /// into the single `JSONError(400, "Invalid request format")` the Go
    /// `ShouldBindJSON` branch produces (auth.go:49-53).
    pub fn passes_binding(&self) -> bool {
        !self.email.is_empty() && is_valid_email(&self.email) && !self.password.is_empty()
    }
}

/// Authenticated user handed back by [`SigninService::authenticate_user`].
///
/// Stands in for Go's `*ent.User` at the handler boundary:
/// * `id` is the numeric user id that feeds the JWT subject — conduit-auth
///   `Claims.user_id` is `i64` (`crates/conduit-auth/src/jwt.rs`), matching
///   Go's numeric `user_id` claim (`biz/auth.go:109`);
/// * `info` is the frontend-contract shape Go builds with
///   `biz.ConvertUserToUserInfo(ctx, user)` (auth.go:76, biz/user.go:279).
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticatedUser {
    pub id: i64,
    pub info: UserInfo,
}

/// Error surface of [`SigninService`]. Mirrors the only two branches the Go
/// handler distinguishes (auth.go:57-66).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigninError {
    /// Go `biz.ErrInvalidPassword` (`biz/errors.go:13`) — unknown email or
    /// wrong password (`biz/auth.go:137,151`). Maps to 401.
    InvalidCredentials,
    /// The credentials identify an account that an administrator has
    /// deactivated. This is an expected authentication rejection, not an
    /// infrastructure failure.
    UserInactive,
    /// Any other service failure. The payload is log-only; the client always
    /// receives the fixed `"Internal server error"` 500 (auth.go:63,71).
    Internal(String),
}

/// Minimal signin-service trait consumed by the handler. Stands in for
/// Go `*biz.AuthService` (only the members `api/auth.go` touches).
#[async_trait::async_trait]
pub trait SigninService: Send + Sync {
    /// Mirrors `AuthService.AuthenticateUser` (`biz/auth.go:122-156`): resolve
    /// the user by email and verify the bcrypt password.
    async fn authenticate_user(
        &self,
        email: &str,
        password: &str,
    ) -> Result<AuthenticatedUser, SigninError>;

    /// Mirrors `AuthService.GenerateJWTToken` (`biz/auth.go:100-119`). The
    /// concrete impl signs HS256 with the system secret via conduit-auth
    /// `encode_hs256` (its token type is `String`).
    async fn generate_jwt_token(&self, user: &AuthenticatedUser) -> Result<String, SigninError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignupError {
    EmailTaken,
    SystemNotInitialized,
    RateLimited,
    Internal(String),
}

/// Registration is separate from signin so deployments can choose whether to
/// wire public signup without weakening the credential-verification service.
#[async_trait::async_trait]
pub trait SignupService: Send + Sync {
    /// Creates an activated non-system-owner user with a private workspace and
    /// workspace Owner role, then returns the frontend authentication shape.
    async fn register_user(&self, request: SignUpRequest)
    -> Result<AuthenticatedUser, SignupError>;
}

/// `POST /admin/auth/signin` — Go `AuthHandlers.SignIn` (auth.go:43-81,
/// mounted unauthenticated at routes.go:88).
///
/// Response table (verbatim Go, all errors in the `JSONError` shape):
///
/// | condition                  | status | body |
/// |----------------------------|--------|------|
/// | bind/validation failure    | 400    | `{"error":{"type":"Bad Request","message":"Invalid request format"}}` (auth.go:49-53) |
/// | `ErrInvalidPassword`       | 401    | `{"error":{"type":"Unauthorized","message":"Invalid email or password"}}` (auth.go:58-61) |
/// | other authenticate failure | 500    | `{"error":{"type":"Internal Server Error","message":"Internal server error"}}` (auth.go:63) |
/// | token generation failure   | 500    | same as above (auth.go:69-73) |
/// | success                    | 200    | `{"user": <UserInfo>, "token": "<jwt>"}` (auth.go:75-80) |
pub async fn sign_in(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // gin `ShouldBindJSON` failure — unreadable body, malformed JSON, or a
    // binding-tag violation — all collapse to the same 400 (auth.go:49-53).
    let request = match body
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SignInRequest>(&bytes).ok())
        .filter(SignInRequest::passes_binding)
    {
        Some(request) => request,
        None => return json_error(StatusCode::BAD_REQUEST, "Invalid request format"),
    };

    let Some(service) = state.services().signin_service() else {
        // Unwired service (Rust-only state; fx guarantees injection in Go)
        // degrades to the generic 500 branch.
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    // auth.go:55-66 — authenticate; ErrInvalidPassword → 401, else → 500.
    let user = match service
        .authenticate_user(&request.email, &request.password)
        .await
    {
        Ok(user) => user,
        Err(SigninError::InvalidCredentials) => {
            return json_error(StatusCode::UNAUTHORIZED, "Invalid email or password");
        }
        Err(SigninError::UserInactive) => {
            return json_error(StatusCode::FORBIDDEN, "Account is deactivated");
        }
        Err(SigninError::Internal(_)) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    // auth.go:68-73 — mint the JWT.
    let token = match service.generate_jwt_token(&user).await {
        Ok(token) => token,
        Err(_) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    // auth.go:75-80 — `SignInResponse{User: ConvertUserToUserInfo(...), Token: token}`
    // via the S06 pure builder.
    (
        StatusCode::OK,
        Json(build_signin_response(token, user.info)),
    )
        .into_response()
}

/// `POST /admin/auth/signup` — public self-registration. Successful signup
/// returns the same `{ user, token }` payload as signin so the frontend can
/// enter the authenticated application without a second password round-trip.
pub async fn sign_up(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // Password signup is a public account-creation and bcrypt endpoint. Keep
    // it unavailable unless the operator explicitly opts in; check before
    // parsing the request so disabled deployments do no attacker-controlled
    // work beyond normal HTTP framing.
    if !state.config().api_auth.allow_password_signup {
        return json_error(StatusCode::FORBIDDEN, "Password signup is disabled");
    }

    let request = match body
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SignUpRequest>(&bytes).ok())
        .filter(SignUpRequest::passes_binding)
    {
        Some(mut request) => {
            request.email = request.email.trim().to_lowercase();
            request
        }
        None => return json_error(StatusCode::BAD_REQUEST, "Invalid request format"),
    };

    let (Some(signup), Some(signin)) = (
        state.services().signup_service(),
        state.services().signin_service(),
    ) else {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
    };

    let user = match signup.register_user(request).await {
        Ok(user) => user,
        Err(SignupError::EmailTaken) => {
            return json_error(StatusCode::CONFLICT, "Email is already registered");
        }
        Err(SignupError::SystemNotInitialized) => {
            return json_error(StatusCode::CONFLICT, "System is not initialized");
        }
        Err(SignupError::RateLimited) => {
            return json_error(StatusCode::TOO_MANY_REQUESTS, "Too many signup attempts");
        }
        Err(SignupError::Internal(_)) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
    };

    let token = match signin.generate_jwt_token(&user).await {
        Ok(token) => token,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error"),
    };

    (
        StatusCode::CREATED,
        Json(build_signin_response(token, user.info)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, header};
    use conduit_config::AppConfig;
    use conduit_core::objects::user::RoleInfo;
    use serde_json::{Value, json};
    use tower::Service;

    use super::*;
    use crate::app_state::AppServices;
    use crate::router::build_router;

    /// Configurable fake standing in for `biz.AuthService`.
    #[derive(Default)]
    struct FakeSigninService {
        fail_authenticate: bool,
        fail_token: bool,
    }

    #[derive(Default)]
    struct FakeSignupService {
        email_taken: bool,
        system_not_initialized: bool,
        rate_limited: bool,
    }

    /// Frontend-compatible sample user (same golden shape as the S06 tests in
    /// `admin_handlers.rs`, mirroring Go `objects.UserInfo`, user.go:3-16).
    fn sample_user() -> UserInfo {
        UserInfo {
            id: "1".to_string(),
            email: "owner@example.com".to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            is_owner: true,
            prefer_language: "en".to_string(),
            avatar: None,
            scopes: vec!["read".to_string()],
            roles: vec![RoleInfo {
                name: "admin".to_string(),
            }],
            ..UserInfo::default()
        }
    }

    #[async_trait::async_trait]
    impl SigninService for FakeSigninService {
        async fn authenticate_user(
            &self,
            email: &str,
            password: &str,
        ) -> Result<AuthenticatedUser, SigninError> {
            if email == "disabled@example.com" {
                return Err(SigninError::UserInactive);
            }
            if self.fail_authenticate {
                return Err(SigninError::Internal("db down".to_string()));
            }
            if email == "owner@example.com" && password == "secret123" {
                Ok(AuthenticatedUser {
                    id: 1,
                    info: sample_user(),
                })
            } else {
                Err(SigninError::InvalidCredentials)
            }
        }

        async fn generate_jwt_token(
            &self,
            user: &AuthenticatedUser,
        ) -> Result<String, SigninError> {
            if self.fail_token {
                return Err(SigninError::Internal("no secret key".to_string()));
            }
            Ok(format!("jwt-for-{}", user.id))
        }
    }

    #[async_trait::async_trait]
    impl SignupService for FakeSignupService {
        async fn register_user(
            &self,
            request: SignUpRequest,
        ) -> Result<AuthenticatedUser, SignupError> {
            if self.email_taken {
                return Err(SignupError::EmailTaken);
            }
            if self.system_not_initialized {
                return Err(SignupError::SystemNotInitialized);
            }
            if self.rate_limited {
                return Err(SignupError::RateLimited);
            }
            Ok(AuthenticatedUser {
                id: 2,
                info: UserInfo {
                    id: "2".to_string(),
                    email: request.email,
                    first_name: request.first_name,
                    last_name: request.last_name,
                    prefer_language: "en".to_string(),
                    has_password: true,
                    ..UserInfo::default()
                },
            })
        }
    }

    fn app_with(service: FakeSigninService) -> Router {
        let services = AppServices::new().with_signin_service(Arc::new(service));
        build_router(AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    fn app_with_signup(signup: FakeSignupService) -> Router {
        app_with_signup_config(signup, true)
    }

    fn app_with_signup_config(signup: FakeSignupService, allow_password_signup: bool) -> Router {
        let services = AppServices::new()
            .with_signin_service(Arc::new(FakeSigninService::default()))
            .with_signup_service(Arc::new(signup));
        let mut config = AppConfig::default();
        config.api_auth.allow_password_signup = allow_password_signup;
        build_router(AppState::new(Arc::new(config), Arc::new(services)))
    }

    async fn post_signin(
        app: &mut Router,
        payload: &str,
    ) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/admin/auth/signin")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    async fn post_signup(
        app: &mut Router,
        payload: &str,
    ) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/admin/auth/signup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    #[tokio::test]
    async fn signup_happy_path_returns_created_user_and_token() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with_signup(FakeSignupService::default());
        let payload = json!({
            "email": "NEW@Example.com",
            "password": "Password123!",
            "firstName": "New",
            "lastName": "User"
        })
        .to_string();
        let (status, body) = post_signup(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["token"], "jwt-for-2");
        assert_eq!(body["user"]["email"], "new@example.com");
        assert_eq!(body["user"]["firstName"], "New");
        assert_eq!(body["user"]["hasPassword"], true);
        Ok(())
    }

    #[tokio::test]
    async fn signup_rejects_invalid_email_and_short_password() -> Result<(), Box<dyn StdError>> {
        for payload in [
            json!({"email": "invalid", "password": "Password123!"}).to_string(),
            json!({"email": "new@example.com", "password": "short"}).to_string(),
        ] {
            let mut app = app_with_signup(FakeSignupService::default());
            let (status, body) = post_signup(&mut app, &payload).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"]["message"], "Invalid request format");
        }
        Ok(())
    }

    #[tokio::test]
    async fn signup_duplicate_email_returns_conflict() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with_signup(FakeSignupService {
            email_taken: true,
            ..Default::default()
        });
        let payload = json!({"email": "used@example.com", "password": "Password123!"}).to_string();
        let (status, body) = post_signup(&mut app, &payload).await?;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["message"], "Email is already registered");
        Ok(())
    }

    #[tokio::test]
    async fn password_signup_is_disabled_by_default() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with_signup_config(FakeSignupService::default(), false);
        let payload = json!({
            "email": "new@example.com",
            "password": "Password123!"
        })
        .to_string();

        let (status, body) = post_signup(&mut app, &payload).await?;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["message"], "Password signup is disabled");
        Ok(())
    }

    #[tokio::test]
    async fn saturated_signup_service_returns_too_many_requests() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with_signup(FakeSignupService {
            rate_limited: true,
            ..Default::default()
        });
        let payload = json!({
            "email": "new@example.com",
            "password": "Password123!"
        })
        .to_string();

        let (status, body) = post_signup(&mut app, &payload).await?;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["message"], "Too many signup attempts");
        Ok(())
    }

    /// Mirrors the success path (auth.go:75-80): 200 with `user` (camelCase
    /// UserInfo, user.go:3-16) and `token`.
    #[tokio::test]
    async fn signin_happy_path_returns_user_and_token() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeSigninService::default());
        let payload = json!({"email": "owner@example.com", "password": "secret123"}).to_string();
        let (status, body) = post_signin(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["token"], "jwt-for-1");
        assert_eq!(body["user"]["id"], "1");
        assert_eq!(body["user"]["email"], "owner@example.com");
        assert_eq!(body["user"]["firstName"], "Ada");
        assert_eq!(body["user"]["lastName"], "Lovelace");
        assert_eq!(body["user"]["isOwner"], true);
        assert_eq!(body["user"]["preferLanguage"], "en");
        assert_eq!(body["user"]["scopes"][0], "read");
        assert_eq!(body["user"]["roles"][0]["name"], "admin");
        // `avatar,omitempty` — absent when nil (user.go:10).
        assert!(body["user"].get("avatar").is_none() || body["user"]["avatar"].is_null());
        Ok(())
    }

    /// Mirrors the ShouldBindJSON failure branch (auth.go:49-53): malformed
    /// JSON and every binding-tag violation return the same JSONError 400.
    #[tokio::test]
    async fn signin_bind_failures_return_invalid_request_format() -> Result<(), Box<dyn StdError>> {
        let cases: Vec<String> = vec![
            "{".to_string(),                                               // malformed JSON
            json!({"password": "secret123"}).to_string(), // missing email (required)
            json!({"email": "not-an-email", "password": "x"}).to_string(), // email tag
            json!({"email": "owner@example.com"}).to_string(), // missing password (required)
        ];

        for payload in cases {
            let mut app = app_with(FakeSigninService::default());
            let (status, body) = post_signin(&mut app, &payload).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
            assert_eq!(
                body,
                json!({"error": {"type": "Bad Request", "message": "Invalid request format"}}),
                "{payload}"
            );
        }
        Ok(())
    }

    /// Mirrors the ErrInvalidPassword branch (auth.go:58-61): wrong password
    /// (and unknown email — biz/auth.go:137 wraps both) → 401.
    #[tokio::test]
    async fn signin_wrong_credentials_return_unauthorized() -> Result<(), Box<dyn StdError>> {
        for payload in [
            json!({"email": "owner@example.com", "password": "wrong-pass"}).to_string(),
            json!({"email": "ghost@example.com", "password": "secret123"}).to_string(),
        ] {
            let mut app = app_with(FakeSigninService::default());
            let (status, body) = post_signin(&mut app, &payload).await?;

            assert_eq!(status, StatusCode::UNAUTHORIZED, "{payload}");
            assert_eq!(
                body,
                json!({"error": {"type": "Unauthorized", "message": "Invalid email or password"}}),
                "{payload}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn signin_deactivated_account_returns_forbidden() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeSigninService::default());
        let payload = json!({"email": "disabled@example.com", "password": "secret123"}).to_string();
        let (status, body) = post_signin(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            body,
            json!({"error": {"type": "Forbidden", "message": "Account is deactivated"}})
        );
        Ok(())
    }

    /// Mirrors the generic authenticate failure branch (auth.go:63).
    #[tokio::test]
    async fn signin_internal_auth_failure_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeSigninService {
            fail_authenticate: true,
            ..FakeSigninService::default()
        });
        let payload = json!({"email": "owner@example.com", "password": "secret123"}).to_string();
        let (status, body) = post_signin(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"type": "Internal Server Error", "message": "Internal server error"}})
        );
        Ok(())
    }

    /// Mirrors the GenerateJWTToken failure branch (auth.go:69-73): same fixed
    /// 500 body as the authenticate failure.
    #[tokio::test]
    async fn signin_token_failure_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeSigninService {
            fail_token: true,
            ..FakeSigninService::default()
        });
        let payload = json!({"email": "owner@example.com", "password": "secret123"}).to_string();
        let (status, body) = post_signin(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"type": "Internal Server Error", "message": "Internal server error"}})
        );
        Ok(())
    }

    /// Rust-only skeleton path: no wired service degrades to the generic 500
    /// branch instead of panicking. (Binding still runs first, so a valid body
    /// is required to reach it.)
    #[tokio::test]
    async fn signin_unwired_service_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = build_router(AppState::default());
        let payload = json!({"email": "owner@example.com", "password": "secret123"}).to_string();
        let (status, body) = post_signin(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Internal server error");
        Ok(())
    }
}

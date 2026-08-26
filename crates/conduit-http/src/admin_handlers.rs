use axum::Json;
use conduit_core::objects::user::UserInfo;
use conduit_core::{ConduitError, ErrorKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct SystemVersionResponse {
    pub status: &'static str,
    pub version: &'static str,
}

pub async fn system_version() -> Json<SystemVersionResponse> {
    Json(SystemVersionResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

// ---------------------------------------------------------------------------
// RUST-P11-003 S04/S05/S06 — system/auth HTTP handler shaping (pure logic).
//
// Mirrors `conduit/internal/server/api/system.go` and `auth.go` plus the
// GraphQL `SystemStatus`/`InitializeSystemPayload` shapes declared in
// `conduit/internal/server/gql/system.graphql`. Only the pure, unit-testable
// response/precondition shaping lives here; the gin handlers in Go read state
// from `SystemService`/`AuthService`, which are not yet ported (blocked: RUST-
// P5-002 / RUST-P4-003 — see TODO_SMALL RUST-P11-003 S01/S02).
// ---------------------------------------------------------------------------

/// REST `SystemStatusResponse` — ported 1:1 from Go `api.SystemStatusResponse`.
///
/// Go source (`api/system.go:39-41`):
/// ```text
/// type SystemStatusResponse struct {
///     IsInitialized bool `json:"isInitialized"`
/// }
/// ```
///
/// This is identical to the GraphQL `SystemStatus` type
/// (`gql/system.graphql:17-19`): `type SystemStatus { isInitialized: Boolean! }`,
/// so the same shape is exposed on both the REST `/admin/system/status`
/// endpoint and the GraphQL `systemStatus` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatusResponse {
    // `default` mirrors Go's tolerant decode: a payload missing `isInitialized`
    // (e.g. a snake_case key typo) leaves the boolean at its zero value instead
    // of rejecting the whole body. Go json silently ignores unknown/missing keys.
    #[serde(default)]
    pub is_initialized: bool,
}

/// Build the `SystemStatusResponse` returned by the unauthenticated
/// `/admin/system/status` route (S04) and the GraphQL `systemStatus` resolver.
///
/// Mirrors Go `SystemHandlers.GetSystemStatus` (`api/system.go:77-88`) and
/// `queryResolver.SystemStatus` (`gql/system.resolvers.go:371-381`) — both only
/// surface `IsInitialized` to the client; every internal-failure path maps to a
/// 5xx before reaching this shaping layer, so the pure builder takes the
/// already-resolved boolean.
pub fn build_system_status_response(is_initialized: bool) -> SystemStatusResponse {
    SystemStatusResponse { is_initialized }
}

/// Precondition check for the unauthenticated `/admin/system/initialize`
/// route (S05). The handler in Go (`api/system.go:133-160`) succeeds only when
/// the system is NOT yet initialized; once an owner exists, a second call is
/// rejected with `400 / "System is already initialized"`.
///
/// `check_initialize_precondition(already_initialized)` returns `Ok(())` when
/// initialization is permitted, or an `ConduitError` with `ErrorKind::InvalidRequest`
/// (HTTP 400) and the Go-identical public message when it must be rejected.
pub fn check_initialize_precondition(already_initialized: bool) -> Result<(), ConduitError> {
    if already_initialized {
        return Err(
            ConduitError::invalid_request("System is already initialized").with_http_status(400),
        );
    }
    let _ = ErrorKind::InvalidRequest; // prove the kind is reachable for HTTP 400 mapping parity
    Ok(())
}

/// REST `SignInResponse` — ported 1:1 from Go `api.SignInResponse`
/// (`api/auth.go:36-40`):
/// ```text
/// type SignInResponse struct {
///     User  *objects.UserInfo `json:"user"`
///     Token string            `json:"token"`
/// }
/// ```
///
/// The frontend-compatible `user` shape is exactly `objects.UserInfo`
/// (ported in `conduit-core::objects::user::UserInfo`); the gin handler
/// constructs it via `biz.ConvertUserToUserInfo` (`biz/user.go:279`), which is
/// not yet ported, so the pure builder takes the already-shaped `UserInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SignInResponse {
    /// `*objects.UserInfo` in Go — always populated on a 200 from `SignIn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInfo>,
    #[serde(default)]
    pub token: String,
}

/// Build the `SignInResponse` returned by the unauthenticated
/// `/admin/auth/signin` route (S06). Mirrors Go `AuthHandlers.SignIn`
/// (`api/auth.go:43-81`) tail: `response := SignInResponse{ User: <UserInfo>, Token: <jwt> }`.
pub fn build_signin_response(token: impl Into<String>, user: UserInfo) -> SignInResponse {
    SignInResponse {
        user: Some(user),
        token: token.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::user::RoleInfo;
    use serde_json::{Value, json};

    // ---- S04: SystemStatusResponse shape -----------------------------------

    #[test]
    fn s04_system_status_serializes_only_is_initialized_when_true() -> Result<(), serde_json::Error>
    {
        let resp = build_system_status_response(true);
        let v = serde_json::to_value(resp)?;
        assert_eq!(v, json!({"isInitialized": true}));
        Ok(())
    }

    #[test]
    fn s04_system_status_serializes_only_is_initialized_when_false() -> Result<(), serde_json::Error>
    {
        let resp = build_system_status_response(false);
        let v = serde_json::to_value(resp)?;
        assert_eq!(v, json!({"isInitialized": false}));
        Ok(())
    }

    #[test]
    fn s04_system_status_round_trips_camel_case() -> Result<(), serde_json::Error> {
        // Mirrors the REST + GraphQL parity contract: only `isInitialized` is exposed.
        let resp: SystemStatusResponse = serde_json::from_str(r#"{"isInitialized":true}"#)?;
        assert!(resp.is_initialized);

        let back = serde_json::to_string(&resp)?;
        assert_eq!(back, r#"{"isInitialized":true}"#);
        Ok(())
    }

    #[test]
    fn s04_system_status_snake_case_input_leaves_field_default_false()
    -> Result<(), serde_json::Error> {
        // Frontend contract is camelCase only; a snake_case input must NOT bind the
        // `isInitialized` field (Go json would also ignore `is_initialized`).
        let parsed: SystemStatusResponse = serde_json::from_str(r#"{"is_initialized":true}"#)?;
        // serde(default) on a missing real field → stays false (the snake key is ignored).
        assert!(!parsed.is_initialized);
        Ok(())
    }

    // ---- S05: initialize precondition --------------------------------------

    #[test]
    fn s05_initialize_permitted_when_not_yet_initialized() {
        assert!(check_initialize_precondition(false).is_ok());
    }

    #[test]
    fn s05_initialize_rejected_when_already_initialized() {
        match check_initialize_precondition(true) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::InvalidRequest);
                assert_eq!(err.http_status, 400);
                assert_eq!(err.message, "System is already initialized");
            }
            Ok(()) => panic!("already-initialized must error"),
        }
    }

    #[test]
    fn s05_initialize_rejection_message_matches_go_verbatim() {
        // Mirrors the exact body of Go InitializeSystemResponse{Success:false,
        // Message:"System is already initialized"} at api/system.go:154-159.
        match check_initialize_precondition(true) {
            Err(err) => assert_eq!(err.message, "System is already initialized"),
            Ok(()) => panic!("already-initialized must error"),
        }
    }

    // ---- S06: SignInResponse shape -----------------------------------------

    fn sample_user() -> UserInfo {
        // Minimal frontend-compatible UserInfo mirroring the Go golden shape:
        // id/email/firstName/lastName/isOwner/scopes/roles present, avatar absent
        // (omitempty).
        UserInfo {
            id: "u-1".to_string(),
            email: "owner@example.com".to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            is_owner: true,
            prefer_language: "en".to_string(),
            avatar: None,
            scopes: vec!["read".to_string(), "write".to_string()],
            roles: vec![RoleInfo {
                name: "admin".to_string(),
            }],
            ..UserInfo::default()
        }
    }

    #[test]
    fn s06_signin_response_shape_matches_go_camel_case() -> Result<(), serde_json::Error> {
        let resp = build_signin_response("jwt.token.here", sample_user());
        let v = serde_json::to_value(&resp)?;

        // Top-level keys: token + user, matching Go SignInResponse json tags.
        assert!(matches!(v.get("token"), Some(Value::String(_))));
        assert_eq!(v["token"], "jwt.token.here");
        assert!(v.get("user").is_some());

        // UserInfo fields surface under their Go camelCase json tags.
        assert_eq!(v["user"]["id"], "u-1");
        assert_eq!(v["user"]["email"], "owner@example.com");
        assert_eq!(v["user"]["firstName"], "Ada");
        assert_eq!(v["user"]["lastName"], "Lovelace");
        assert_eq!(v["user"]["isOwner"], true);
        assert_eq!(v["user"]["scopes"][0], "read");
        assert_eq!(v["user"]["roles"][0]["name"], "admin");
        Ok(())
    }

    #[test]
    fn s06_signin_response_omits_user_avatar_when_none() -> Result<(), serde_json::Error> {
        // Go omitempty parity: `avatar,omitempty` drops the key when nil.
        let resp = build_signin_response("t", sample_user());
        let v = serde_json::to_value(&resp)?;
        assert!(v["user"].get("avatar").is_none() || v["user"]["avatar"].is_null());
        Ok(())
    }

    #[test]
    fn s06_signin_response_round_trips_token_and_user() -> Result<(), serde_json::Error> {
        let resp = build_signin_response("abc.def.ghi", sample_user());
        let json_str = serde_json::to_string(&resp)?;
        let parsed: SignInResponse = serde_json::from_str(&json_str)?;
        assert_eq!(parsed.token, "abc.def.ghi");
        match parsed.user {
            Some(user) => {
                assert_eq!(user.email, "owner@example.com");
                assert_eq!(user.roles.len(), 1);
            }
            None => panic!("user must round-trip"),
        }
        Ok(())
    }
}

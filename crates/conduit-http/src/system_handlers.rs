//! System REST handlers: `GET /admin/system/status` + `POST
//! /admin/system/initialize` (RUST-P11-003 S10 — frontend smoke entry).
//!
//! Ports the gin handler bodies of `conduit/internal/server/api/system.go`:
//! `GetSystemStatus` (system.go:77-88) and `InitializeSystem`
//! (system.go:133-184). The pure response/precondition shaping (S04/S05)
//! already lives in [`crate::admin_handlers`]; this module adds the axum
//! handlers plus the minimal [`SystemService`] trait the host wires at boot
//! (Go injects `*biz.SystemService` via fx — `system.go:22-36`).
//!
//! Out of scope here (already/elsewhere ported): `Health` (system.go:91-101 →
//! `health.rs`), `WebhookEcho` (system.go:104-130 → `webhook_handlers.rs`).
//! `GetFavicon` (system.go:187-250) lives below (RUST-P11-003 S03): brand-logo
//! data-URL decode plus the embedded default favicon — the *same* asset file
//! Go embeds (`//go:embed favicon.ico`, `internal/server/assets/favicon.go:7-8`).

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::admin_handlers::{build_system_status_response, check_initialize_precondition};
use crate::api_error::{is_valid_email, json_error, min_chars};
use crate::app_state::AppState;

/// Service-boundary params for system initialization. Mirrors Go
/// `biz.InitializeSystemParams` (`biz/system.go:646-653`) field-for-field.
/// The handler copies the bound request into this struct verbatim
/// (system.go:163-170) — including an empty `prefer_language`; the
/// "default to en" rule belongs to the service (`biz/system.go:693-696`),
/// not the handler.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitializeSystemParams {
    pub owner_email: String,
    pub owner_password: String,
    pub owner_first_name: String,
    pub owner_last_name: String,
    pub brand_name: String,
    pub prefer_language: String,
}

/// Minimal system-service trait consumed by the two handlers. Stands in for
/// Go `*biz.SystemService` (only the members `api/system.go` touches):
///
/// * [`is_initialized`](Self::is_initialized) ← `SystemService.IsInitialized`
///   (`biz/system.go:588`),
/// * [`initialize`](Self::initialize) ← `SystemService.Initialize`
///   (`biz/system.go:656`),
/// * [`brand_logo`](Self::brand_logo) ← `SystemService.BrandLogo`
///   (`biz/system.go:833-848`).
///
/// Error payloads are plain `String`s: the Go handler only ever logs the error
/// or embeds it via `%v` into `"Failed to initialize system: %v"`
/// (system.go:174), so no richer error type is needed at this boundary. The
/// concrete implementation (conduit-services `SystemService`) is wired by the
/// host binary; this crate never depends on it directly.
#[async_trait::async_trait]
pub trait SystemService: Send + Sync {
    async fn is_initialized(&self) -> Result<bool, String>;
    async fn initialize(&self, params: InitializeSystemParams) -> Result<(), String>;
    /// The brand logo as a base64 data URL, or `""` when unset — ent NotFound
    /// yields `("", nil)` in Go (`biz/system.go:838-842`).
    async fn brand_logo(&self) -> Result<String, String>;

    /// The HS256 JWT signing secret as raw bytes, or `None` when the system is
    /// not yet initialized (no secret persisted).
    ///
    /// Mirrors Go `SystemService.SecretKey` (`biz/system.go:783-794`), which
    /// the JWT auth middleware reads on every request via
    /// `AuthService.AuthenticateJWTToken` (`biz/auth.go:161-169`). This lets
    /// the Rust JWT middleware source the secret dynamically from the DB
    /// (written at `initialize` time) instead of a static boot-time config
    /// value — the two must match for signed tokens to validate.
    ///
    /// The returned bytes are the *signing* bytes (already decoded from the
    /// hex-encoded storage form), so they line up with the bytes the signin
    /// service uses when minting tokens.
    ///
    /// A default `Ok(None)` implementation keeps existing fakes/back-compat
    /// paths (config-based secret) working without change.
    async fn jwt_secret(&self) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    /// IP addresses and CIDR prefixes blocked from external API routes.
    ///
    /// A default empty list keeps lightweight test services compatible while
    /// the production implementation loads the setting from PostgreSQL for
    /// every request, matching Go's `SecuritySettingsOrDefault` behavior.
    async fn blocked_ips(&self) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
}

/// `InitializeSystemRequest` — ported 1:1 from Go (`api/system.go:53-60`):
///
/// ```text
/// OwnerEmail     string `json:"ownerEmail"     binding:"required,email"`
/// OwnerPassword  string `json:"ownerPassword"  binding:"required,min=6"`
/// OwnerFirstName string `json:"ownerFirstName" binding:"required"`
/// OwnerLastName  string `json:"ownerLastName"  binding:"required"`
/// BrandName      string `json:"brandName"      binding:"required"`
/// PreferLanguage string `json:"preferLanguage,omitempty"`
/// ```
///
/// `#[serde(default)]` mirrors Go's decode-then-validate order: a missing key
/// binds the zero value and the `required` check (in
/// [`passes_binding`](Self::passes_binding)) rejects it, exactly like gin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeSystemRequest {
    #[serde(default)]
    pub owner_email: String,
    #[serde(default)]
    pub owner_password: String,
    #[serde(default)]
    pub owner_first_name: String,
    #[serde(default)]
    pub owner_last_name: String,
    #[serde(default)]
    pub brand_name: String,
    #[serde(default)]
    pub prefer_language: String,
}

impl InitializeSystemRequest {
    /// Enforce the gin `binding:"..."` tags (system.go:54-59). Any failure
    /// collapses into the single `"Invalid request format"` 400 the Go
    /// `ShouldBindJSON` branch produces (system.go:136-144).
    pub fn passes_binding(&self) -> bool {
        !self.owner_email.is_empty()
            && is_valid_email(&self.owner_email)
            && !self.owner_password.is_empty()
            && min_chars(&self.owner_password, 6)
            && !self.owner_first_name.is_empty()
            && !self.owner_last_name.is_empty()
            && !self.brand_name.is_empty()
    }
}

/// `InitializeSystemResponse` — ported 1:1 from Go (`api/system.go:63-66`):
/// `{"success": bool, "message": string}`. Used for both the 200 success body
/// and the 400/500 failure bodies of the initialize endpoint (the status-check
/// 500 is the lone exception — it uses the `JSONError` shape, system.go:149).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeSystemResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub message: String,
}

/// `GET /admin/system/status` — Go `SystemHandlers.GetSystemStatus`
/// (system.go:77-88, mounted unauthenticated at routes.go:85).
///
/// * service error → `JSONError(500, "Failed to check system status")`
///   (system.go:79-83);
/// * success → `200 {"isInitialized": bool}` (system.go:85-87) via the S04
///   pure builder.
///
/// An unwired service (Rust-only state; fx guarantees injection in Go)
/// degrades to the same 500 error branch.
pub async fn get_system_status(State(state): State<AppState>) -> Response {
    let Some(service) = state.services().system_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to check system status",
        );
    };

    match service.is_initialized().await {
        Ok(is_initialized) => (
            StatusCode::OK,
            Json(build_system_status_response(is_initialized)),
        )
            .into_response(),
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to check system status",
        ),
    }
}

/// `POST /admin/system/initialize` — Go `SystemHandlers.InitializeSystem`
/// (system.go:133-184, mounted unauthenticated at routes.go:86).
///
/// Response table (verbatim Go):
///
/// | condition                    | status | body |
/// |------------------------------|--------|------|
/// | bind/validation failure      | 400    | `{"success":false,"message":"Invalid request format"}` (system.go:137-144) |
/// | IsInitialized errors         | 500    | `{"error":{"type":"Internal Server Error","message":"Failed to check initialization status"}}` (system.go:147-151) |
/// | already initialized          | 400    | `{"success":false,"message":"System is already initialized"}` (system.go:153-160) |
/// | Initialize errors            | 500    | `{"success":false,"message":"Failed to initialize system: <err>"}` (system.go:171-178) |
/// | success                      | 200    | `{"success":true,"message":"System initialized successfully"}` (system.go:180-183) |
pub async fn initialize_system(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // gin `ShouldBindJSON` failure — unreadable body, malformed JSON, or a
    // binding-tag violation — all collapse to the same 400 (system.go:136-144).
    let request = match body
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InitializeSystemRequest>(&bytes).ok())
        .filter(InitializeSystemRequest::passes_binding)
    {
        Some(request) => request,
        None => {
            return initialize_response(StatusCode::BAD_REQUEST, false, "Invalid request format");
        }
    };

    let Some(service) = state.services().system_service() else {
        // Unwired service degrades to the IsInitialized error branch below.
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to check initialization status",
        );
    };

    // system.go:146-151 — pre-check current state.
    let is_initialized = match service.is_initialized().await {
        Ok(is_initialized) => is_initialized,
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to check initialization status",
            );
        }
    };

    // system.go:153-160 — S05 precondition; the helper single-sources the
    // Go-verbatim "System is already initialized" message.
    if let Err(err) = check_initialize_precondition(is_initialized) {
        return initialize_response(StatusCode::BAD_REQUEST, false, err.message);
    }

    // system.go:162-178 — delegate to the service with the bound fields copied
    // verbatim (prefer_language stays raw; the "en" default is biz-side).
    let result = service
        .initialize(InitializeSystemParams {
            owner_email: request.owner_email,
            owner_password: request.owner_password,
            owner_first_name: request.owner_first_name,
            owner_last_name: request.owner_last_name,
            brand_name: request.brand_name,
            prefer_language: request.prefer_language,
        })
        .await;

    match result {
        Ok(()) => initialize_response(
            StatusCode::OK,
            true,
            "System initialized successfully", // system.go:180-183
        ),
        Err(err) => initialize_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            false,
            format!("Failed to initialize system: {err}"), // system.go:172-175
        ),
    }
}

/// Shared `c.JSON(status, InitializeSystemResponse{...})` shorthand.
fn initialize_response(status: StatusCode, success: bool, message: impl Into<String>) -> Response {
    (
        status,
        Json(InitializeSystemResponse {
            success,
            message: message.into(),
        }),
    )
        .into_response()
}

/// Default Conduit API favicon embedded in the server binary.
static DEFAULT_FAVICON: &[u8] = include_bytes!("../assets/favicon.ico");

/// `GET /favicon` — Go `SystemHandlers.GetFavicon` (system.go:186-250,
/// mounted unauthenticated at routes.go:76-77 "Favicon API - DO NOT AUTH").
///
/// | condition                       | status | response |
/// |---------------------------------|--------|----------|
/// | `BrandLogo` error               | —      | log-only in Go (system.go:190-193); flow continues with `""` |
/// | no brand logo                   | 200    | embedded default favicon, `image/x-icon`, `Cache-Control: public, max-age=3600` (system.go:196-208) |
/// | logo without `data:` prefix     | 400    | `JSONError` "Invalid brand logo format" (system.go:212-215) |
/// | comma-split parts != 2          | 400    | same (system.go:218-222) |
/// | header part missing `:` or `;`  | 400    | same (system.go:226-232) |
/// | base64 decode failure           | 400    | `JSONError` "Failed to decode brand logo" (system.go:237-241) |
/// | success                         | 200    | decoded bytes, `Content-Type` from the data URL, same Cache-Control (system.go:244-249) |
///
/// Go's `assets.Favicon.ReadFile` error branch (system.go:197-201, 500
/// "Failed to read default favicon") cannot occur here: `include_bytes!` is
/// resolved at compile time.
pub async fn get_favicon(State(state): State<AppState>) -> Response {
    // system.go:190-193 — a BrandLogo failure is log-only; the flow continues
    // with the zero value "" and therefore serves the default icon. An
    // unwired service (Rust-only skeleton state) behaves the same way.
    let brand_logo = match state.services().system_service() {
        Some(service) => service.brand_logo().await.unwrap_or_default(),
        None => String::new(),
    };

    // system.go:195-208 — no brand logo -> embedded default favicon.
    if brand_logo.is_empty() {
        return binary_response(StatusCode::OK, "image/x-icon", DEFAULT_FAVICON.to_vec());
    }

    // system.go:210-215 — the stored logo must be a data URL
    // ("data:image/png;base64,iVBOR...").
    if !brand_logo.starts_with("data:") {
        return json_error(StatusCode::BAD_REQUEST, "Invalid brand logo format");
    }

    // system.go:217-222 — `strings.Split(brandLogo, ",")` must yield exactly
    // "<header>,<base64>".
    let parts: Vec<&str> = brand_logo.split(',').collect();
    if parts.len() != 2 {
        return json_error(StatusCode::BAD_REQUEST, "Invalid brand logo format");
    }

    // system.go:224-234 — the MIME type sits between the first ':' and the
    // first ';' of the header part ("data:image/png;base64").
    let header_part = parts[0];
    let (Some(mime_start), Some(mime_end)) = (header_part.find(':'), header_part.find(';')) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid brand logo format");
    };
    let Some(mime_type) = header_part.get(mime_start + 1..mime_end) else {
        // Unreachable given the "data:" prefix (the first ':' is at byte 4, so
        // no ';' fits before it). Go would panic-slice here and gin's recovery
        // would answer a bare 500 — mirror that instead of panicking.
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    // system.go:236-241 — Go base64.StdEncoding.
    let Ok(image_data) = base64_std_decode(parts[1]) else {
        return json_error(StatusCode::BAD_REQUEST, "Failed to decode brand logo");
    };

    // system.go:243-249 — Content-Type from the data URL, 1h public cache.
    // (Go also sets Content-Length explicitly at system.go:246; both stacks
    // derive it from the body anyway.)
    binary_response(StatusCode::OK, mime_type, image_data)
}

/// `c.Data(status, contentType, data)` + the two `c.Header` calls the favicon
/// handler makes (system.go:203-205, 244-249).
fn binary_response(status: StatusCode, content_type: &str, body: Vec<u8>) -> Response {
    match Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(body))
    {
        Ok(response) => response,
        // MIME bytes not header-encodable (Go's net/http would drop the value
        // at write time); degrade to a bare 500 instead of panicking.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Standard-alphabet base64 decode mirroring Go `base64.StdEncoding.DecodeString`
/// (system.go:237): 4-char quanta, `=` padding completing only the final
/// quantum, `\r`/`\n` ignored anywhere, every other byte outside the alphabet
/// rejected. (Go's `StdEncoding` — unlike `StrictEncoding` — does not reject
/// non-zero trailing padding bits; neither does this decoder.)
fn base64_std_decode(input: &str) -> Result<Vec<u8>, ()> {
    fn sextet(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let mut decoded = Vec::with_capacity(input.len() / 4 * 3);
    let mut quantum = [0_u8; 4];
    let mut filled = 0_usize;
    let mut padding = 0_usize;
    let mut terminated = false;

    for &byte in input.as_bytes() {
        if byte == b'\r' || byte == b'\n' {
            continue;
        }
        if terminated {
            // Data after a padded quantum — Go: "illegal base64 data".
            return Err(());
        }
        if byte == b'=' {
            // '=' may only complete the final quantum (positions 2 and 3).
            if filled < 2 {
                return Err(());
            }
            quantum[filled] = 0;
            filled += 1;
            padding += 1;
        } else {
            if padding > 0 {
                return Err(());
            }
            let Some(value) = sextet(byte) else {
                return Err(());
            };
            quantum[filled] = value;
            filled += 1;
        }

        if filled == 4 {
            let combined = (u32::from(quantum[0]) << 18)
                | (u32::from(quantum[1]) << 12)
                | (u32::from(quantum[2]) << 6)
                | u32::from(quantum[3]);
            decoded.push((combined >> 16) as u8);
            if padding < 2 {
                decoded.push((combined >> 8) as u8);
            }
            if padding == 0 {
                decoded.push(combined as u8);
            }
            if padding > 0 {
                terminated = true;
            }
            filled = 0;
            padding = 0;
        }
    }

    if filled != 0 {
        // Incomplete final quantum — Go rejects unpadded leftovers.
        return Err(());
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, header};
    use conduit_config::AppConfig;
    use serde_json::{Value, json};
    use tower::Service;

    use super::*;
    use crate::app_state::AppServices;
    use crate::router::build_router;

    /// Configurable fake standing in for `biz.SystemService`.
    #[derive(Default)]
    struct FakeSystemService {
        initialized: bool,
        fail_is_initialized: bool,
        fail_initialize: bool,
        logo: Option<String>,
        fail_brand_logo: bool,
        seen: Mutex<Option<InitializeSystemParams>>,
    }

    #[async_trait::async_trait]
    impl SystemService for FakeSystemService {
        async fn is_initialized(&self) -> Result<bool, String> {
            if self.fail_is_initialized {
                return Err("db down".to_string());
            }
            Ok(self.initialized)
        }

        async fn initialize(&self, params: InitializeSystemParams) -> Result<(), String> {
            if self.fail_initialize {
                return Err("boom".to_string());
            }
            if let Ok(mut guard) = self.seen.lock() {
                *guard = Some(params);
            }
            Ok(())
        }

        async fn brand_logo(&self) -> Result<String, String> {
            if self.fail_brand_logo {
                return Err("db down".to_string());
            }
            Ok(self.logo.clone().unwrap_or_default())
        }
    }

    fn app_with(service: Arc<FakeSystemService>) -> Router {
        let services = AppServices::new().with_system_service(service);
        build_router(AppState::new(
            Arc::new(AppConfig::default()),
            Arc::new(services),
        ))
    }

    async fn call(
        app: &mut Router,
        method: Method,
        uri: &str,
        body: Option<&str>,
    ) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let request = match body {
            Some(payload) => Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))?,
            None => Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())?,
        };
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    fn valid_initialize_body() -> String {
        json!({
            "ownerEmail": "owner@example.com",
            "ownerPassword": "secret123",
            "ownerFirstName": "Ada",
            "ownerLastName": "Lovelace",
            "brandName": "Conduit API",
        })
        .to_string()
    }

    // ---- GET /admin/system/status ---------------------------------------

    /// Mirrors GetSystemStatus success (system.go:85-87): 200 with exactly
    /// `{"isInitialized": <bool>}` for both states.
    #[tokio::test]
    async fn status_returns_is_initialized_bool() -> Result<(), Box<dyn StdError>> {
        for initialized in [false, true] {
            let mut app = app_with(Arc::new(FakeSystemService {
                initialized,
                ..FakeSystemService::default()
            }));
            let (status, body) = call(&mut app, Method::GET, "/admin/system/status", None).await?;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, json!({"isInitialized": initialized}));
        }
        Ok(())
    }

    /// Mirrors the IsInitialized error branch (system.go:79-83):
    /// JSONError(500, "Failed to check system status").
    #[tokio::test]
    async fn status_service_error_returns_go_json_error() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeSystemService {
            fail_is_initialized: true,
            ..FakeSystemService::default()
        }));
        let (status, body) = call(&mut app, Method::GET, "/admin/system/status", None).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"type": "Internal Server Error", "message": "Failed to check system status"}})
        );
        Ok(())
    }

    /// Rust-only skeleton path: no wired service degrades to the same 500
    /// error branch instead of panicking.
    #[tokio::test]
    async fn status_unwired_service_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = build_router(AppState::default());
        let (status, body) = call(&mut app, Method::GET, "/admin/system/status", None).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Failed to check system status");
        Ok(())
    }

    // ---- POST /admin/system/initialize ----------------------------------

    /// Mirrors the success path (system.go:162-183): 200 with the Go-verbatim
    /// body, and the service receives the bound fields unchanged —
    /// `preferLanguage` omitted binds "" (the "en" default is biz-side,
    /// biz/system.go:693-696).
    #[tokio::test]
    async fn initialize_happy_path_calls_service_with_bound_params() -> Result<(), Box<dyn StdError>>
    {
        let service = Arc::new(FakeSystemService::default());
        let mut app = app_with(service.clone());
        let (status, body) = call(
            &mut app,
            Method::POST,
            "/admin/system/initialize",
            Some(&valid_initialize_body()),
        )
        .await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"success": true, "message": "System initialized successfully"})
        );

        let seen = match service.seen.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        };
        assert_eq!(
            seen,
            Some(InitializeSystemParams {
                owner_email: "owner@example.com".to_string(),
                owner_password: "secret123".to_string(),
                owner_first_name: "Ada".to_string(),
                owner_last_name: "Lovelace".to_string(),
                brand_name: "Conduit API".to_string(),
                prefer_language: String::new(),
            })
        );
        Ok(())
    }

    /// `preferLanguage` passes through raw when supplied (system.go:169).
    #[tokio::test]
    async fn initialize_prefer_language_passes_through() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeSystemService::default());
        let mut app = app_with(service.clone());
        let payload = json!({
            "ownerEmail": "owner@example.com",
            "ownerPassword": "secret123",
            "ownerFirstName": "Ada",
            "ownerLastName": "Lovelace",
            "brandName": "Conduit API",
            "preferLanguage": "zh",
        })
        .to_string();
        let (status, _) = call(
            &mut app,
            Method::POST,
            "/admin/system/initialize",
            Some(&payload),
        )
        .await?;

        assert_eq!(status, StatusCode::OK);
        let seen = match service.seen.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        };
        assert_eq!(
            seen.map(|params| params.prefer_language),
            Some("zh".to_string())
        );
        Ok(())
    }

    /// Mirrors the ShouldBindJSON failure branch (system.go:136-144): malformed
    /// JSON and every binding-tag violation return the same 400 body.
    #[tokio::test]
    async fn initialize_bind_failures_return_invalid_request_format()
    -> Result<(), Box<dyn StdError>> {
        let cases: Vec<String> = vec![
            "{".to_string(), // malformed JSON
            json!({"ownerPassword": "secret123", "ownerFirstName": "A", "ownerLastName": "L", "brandName": "B"}).to_string(), // missing ownerEmail (required)
            json!({"ownerEmail": "not-an-email", "ownerPassword": "secret123", "ownerFirstName": "A", "ownerLastName": "L", "brandName": "B"}).to_string(), // email tag
            json!({"ownerEmail": "owner@example.com", "ownerPassword": "12345", "ownerFirstName": "A", "ownerLastName": "L", "brandName": "B"}).to_string(), // min=6
            json!({"ownerEmail": "owner@example.com", "ownerPassword": "secret123", "ownerFirstName": "", "ownerLastName": "L", "brandName": "B"}).to_string(), // required first name
            json!({"ownerEmail": "owner@example.com", "ownerPassword": "secret123", "ownerFirstName": "A", "ownerLastName": "L"}).to_string(), // missing brandName
        ];

        for payload in cases {
            let mut app = app_with(Arc::new(FakeSystemService::default()));
            let (status, body) = call(
                &mut app,
                Method::POST,
                "/admin/system/initialize",
                Some(&payload),
            )
            .await?;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
            assert_eq!(
                body,
                json!({"success": false, "message": "Invalid request format"}),
                "{payload}"
            );
        }
        Ok(())
    }

    /// Mirrors the already-initialized rejection (system.go:153-160).
    #[tokio::test]
    async fn initialize_rejected_when_already_initialized() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeSystemService {
            initialized: true,
            ..FakeSystemService::default()
        }));
        let (status, body) = call(
            &mut app,
            Method::POST,
            "/admin/system/initialize",
            Some(&valid_initialize_body()),
        )
        .await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({"success": false, "message": "System is already initialized"})
        );
        Ok(())
    }

    /// Mirrors the IsInitialized error branch inside initialize
    /// (system.go:147-151): JSONError shape, not InitializeSystemResponse.
    #[tokio::test]
    async fn initialize_status_check_failure_uses_json_error_shape() -> Result<(), Box<dyn StdError>>
    {
        let mut app = app_with(Arc::new(FakeSystemService {
            fail_is_initialized: true,
            ..FakeSystemService::default()
        }));
        let (status, body) = call(
            &mut app,
            Method::POST,
            "/admin/system/initialize",
            Some(&valid_initialize_body()),
        )
        .await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"error": {"type": "Internal Server Error", "message": "Failed to check initialization status"}})
        );
        Ok(())
    }

    /// Mirrors the Initialize error branch (system.go:171-178): the service
    /// error is embedded via `%v` into the message.
    #[tokio::test]
    async fn initialize_service_failure_embeds_error_in_message() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeSystemService {
            fail_initialize: true,
            ..FakeSystemService::default()
        }));
        let (status, body) = call(
            &mut app,
            Method::POST,
            "/admin/system/initialize",
            Some(&valid_initialize_body()),
        )
        .await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({"success": false, "message": "Failed to initialize system: boom"})
        );
        Ok(())
    }

    // ---- GET /favicon (system.go:186-250) --------------------------------

    async fn get_favicon_raw(
        app: &mut Router,
    ) -> Result<(StatusCode, String, String, Vec<u8>), Box<dyn StdError>> {
        let request = Request::builder().uri("/favicon").body(Body::empty())?;
        let response = app.call(request).await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await?;
        Ok((status, content_type, cache_control, bytes.to_vec()))
    }

    /// The default asset is a non-empty ICO file.
    #[test]
    fn default_favicon_embeds_the_conduit_asset() {
        assert!(DEFAULT_FAVICON.len() > 4);
        assert_eq!(&DEFAULT_FAVICON[..4], &[0x00, 0x00, 0x01, 0x00]);
    }

    /// No brand logo → default favicon with the Go headers (system.go:196-208).
    /// A BrandLogo service error (log-only, system.go:190-193) and the unwired
    /// skeleton state behave identically.
    #[tokio::test]
    async fn favicon_serves_default_when_no_brand_logo() -> Result<(), Box<dyn StdError>> {
        let apps = [
            app_with(Arc::new(FakeSystemService::default())), // BrandLogo -> ""
            app_with(Arc::new(FakeSystemService {
                fail_brand_logo: true,
                ..FakeSystemService::default()
            })),
            build_router(AppState::default()), // unwired service
        ];
        for mut app in apps {
            let (status, content_type, cache_control, body) = get_favicon_raw(&mut app).await?;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(content_type, "image/x-icon");
            assert_eq!(cache_control, "public, max-age=3600");
            assert_eq!(body, DEFAULT_FAVICON.to_vec());
        }
        Ok(())
    }

    /// Brand-logo data URL → decoded bytes with the MIME type from the URL
    /// (system.go:210-249). "aGVsbG8=" is base64 for "hello".
    #[tokio::test]
    async fn favicon_serves_decoded_brand_logo() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeSystemService {
            logo: Some("data:image/png;base64,aGVsbG8=".to_string()),
            ..FakeSystemService::default()
        }));
        let (status, content_type, cache_control, body) = get_favicon_raw(&mut app).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "image/png");
        assert_eq!(cache_control, "public, max-age=3600");
        assert_eq!(body, b"hello".to_vec());
        Ok(())
    }

    /// Malformed logo values → JSONError(400, "Invalid brand logo format"):
    /// missing "data:" prefix (system.go:212-215), wrong comma count
    /// (system.go:218-222), no ';' in the header part (system.go:226-232).
    #[tokio::test]
    async fn favicon_invalid_format_returns_json_error() -> Result<(), Box<dyn StdError>> {
        for logo in [
            "https://cdn.example/logo.png",     // no data: prefix
            "data:image/png;base64",            // no comma
            "data:image/png;base64,aGVsbG8=,x", // two commas
            "data:image/png,aGVsbG8=",          // no ';' -> mimeEnd == -1
        ] {
            let mut app = app_with(Arc::new(FakeSystemService {
                logo: Some(logo.to_string()),
                ..FakeSystemService::default()
            }));
            let request = Request::builder().uri("/favicon").body(Body::empty())?;
            let response = app.call(request).await?;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 4096).await?;
            let body: Value = serde_json::from_slice(&bytes)?;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{logo}");
            assert_eq!(
                body,
                json!({"error": {"type": "Bad Request", "message": "Invalid brand logo format"}}),
                "{logo}"
            );
        }
        Ok(())
    }

    /// Undecodable base64 → JSONError(400, "Failed to decode brand logo")
    /// (system.go:237-241).
    #[tokio::test]
    async fn favicon_bad_base64_returns_decode_error() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeSystemService {
            logo: Some("data:image/png;base64,!!!not-base64!!!".to_string()),
            ..FakeSystemService::default()
        }));
        let request = Request::builder().uri("/favicon").body(Body::empty())?;
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&bytes)?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({"error": {"type": "Bad Request", "message": "Failed to decode brand logo"}})
        );
        Ok(())
    }

    /// The hand-rolled decoder matches Go base64.StdEncoding.DecodeString
    /// semantics (accept/reject table).
    #[test]
    fn base64_std_decode_matches_go_std_encoding() {
        // Accepted.
        assert_eq!(base64_std_decode(""), Ok(Vec::new()));
        assert_eq!(base64_std_decode("aGVsbG8="), Ok(b"hello".to_vec()));
        assert_eq!(base64_std_decode("aGVsbG8h"), Ok(b"hello!".to_vec()));
        assert_eq!(base64_std_decode("AA=="), Ok(vec![0x00]));
        assert_eq!(base64_std_decode("/w=="), Ok(vec![0xff]));
        // \r\n ignored anywhere (Go decoder behavior).
        assert_eq!(base64_std_decode("aGVs\r\nbG8=\n"), Ok(b"hello".to_vec()));

        // Rejected: bad alphabet, missing padding, misplaced '=', trailing data.
        assert_eq!(base64_std_decode("!!!!"), Err(()));
        assert_eq!(base64_std_decode("aGVsbG8"), Err(())); // unpadded leftover
        assert_eq!(base64_std_decode("A==="), Err(()));
        assert_eq!(base64_std_decode("AA=A"), Err(()));
        assert_eq!(base64_std_decode("AA==AA=="), Err(())); // data after padding
        // URL-safe alphabet is NOT accepted by StdEncoding.
        assert_eq!(base64_std_decode("a-b_"), Err(()));
    }
}

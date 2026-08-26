//! A01/A02 authorization guards for the OpenAPI GraphQL surface.
//!
//! Mirrors the Go `WithOpenAPIAuth` middleware
//! (`internal/server/middleware/auth.go:117-161`) as pure, HTTP-free
//! predicates:
//!
//! * A01 service-account guard — [`authorize_openapi`]. In Go the check lives
//!   in the middleware (handler 前置), NOT inside any resolver; the Rust
//!   `conduit-http` layer (P11/P2) owns the wiring and the final status code.
//! * A02 project filter — [`apply_project_filter`], the pure encoding of the
//!   `contexts.WithProjectID` injection that scopes every read.
//!
//! Everything here is pure: no DB, no HTTP, no async-graphql server wiring.
//! The crate re-uses `conduit-auth::principal` so the service-account
//! predicate stays the single source of truth across the admin and OpenAPI
//! surfaces.

use conduit_auth::principal::{ApiKeyKind, Principal, PrincipalKind};
use conduit_core::{ConduitError, ErrorKind};

// =============================================================================
// A01 — service-account guard (re-uses conduit-auth::Principal)
// =============================================================================

/// Authorize a principal for the OpenAPI GraphQL surface.
///
/// Mirrors Go `WithOpenAPIAuth` (`internal/server/middleware/auth.go:121-161`):
/// the middleware resolves the API key, then enforces
/// `apiKey.Type == apikey.TypeServiceAccount` (`auth.go:140-143`). Anything
/// else — user keys, noauth fallback, JWT user principals, missing principal —
/// is rejected with a 403 `Forbidden`. The Go middleware actually returns 401
/// "Invalid API key" on the type mismatch (it never injects the principal),
/// but the Rust `conduit-http` middleware layer centralises the 401 path; this
/// crate only owns the post-authentication authorisation decision and so
/// returns 403 here, matching RUST-P12-002 A01 ("非 service-account API key
/// 403").
///
/// This is a pure predicate the http layer (or a defensive resolver) calls on
/// the principal — it does not trust the middleware to have already filtered
/// by `api_key_kind`.
pub fn authorize_openapi(principal: &Principal) -> Result<(), ConduitError> {
    // Only API-key principals reach the OpenAPI surface in production
    // (`WithOpenAPIAuth` never injects user/system principals). We still
    // reject non-API-key principals defensively — Go's middleware chain
    // guarantees it, but a misconfigured router shouldn't leak.
    if !matches!(principal.kind, PrincipalKind::ApiKey) {
        return Err(forbidden_openapi_surface());
    }

    if !principal.api_key_kind.is_service_account() {
        return Err(forbidden_openapi_surface());
    }

    Ok(())
}

fn forbidden_openapi_surface() -> ConduitError {
    // Intentionally generic message: the Go middleware also returns the bare
    // "Invalid API key" string and never echoes back the principal kind, to
    // avoid leaking which principal type was attempted.
    ConduitError::new(
        ErrorKind::Forbidden,
        "openapi graphql requires a service account api key",
    )
}

// =============================================================================
// A02 — project filter
// =============================================================================

/// A GraphQL query document whose selection set has been project-scoped.
///
/// The OpenAPI resolver never hands the raw caller-supplied query to the
/// backing data loader; it first injects the caller's `project_id` so the
/// ent privacy layer (Go) / repository filter (Rust) enforces same-project
/// visibility. This struct encodes that injection as a pure value so the
/// transport layer can carry it through to the loader without re-deriving the
/// scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredQuery {
    /// Original caller-supplied query text (verbatim, for logging/audit).
    pub raw_query: String,
    /// The single project this query is constrained to. Always populated in
    /// production — [`apply_project_filter`] rejects principals without a
    /// `project_id` so a service-account key that somehow lost its project
    /// edge cannot fall through to an unscoped read.
    pub project_id: String,
    /// The principal that authorised this filter. Carried so the loader can
    /// re-assert `api_key_kind == ServiceAccount` immediately before issuing
    /// the underlying read (defence-in-depth double-check).
    pub authorised_by: Principal,
}

/// Apply the API key's `project_id` as a hard filter to a caller query.
///
/// Mirrors the Go privacy-layer behaviour exercised by
/// `TestOpenAPIResolver_APIKey_CrossProjectDenied` /
/// `TestOpenAPIResolver_APIKeyQuotaUsages_CrossProjectDenied_ByID`: the
/// resolver receives the caller's `(id|key|name)` triple, but the underlying
/// `apiKeyService.GetForRead` only ever returns rows whose `project_id`
/// matches the principal injected by `WithOpenAPIAuth` (`auth.go:146-148`
/// sets `contexts.WithProjectID`). Foreign-project identifiers therefore
/// surface as a uniform `NotFound` — no existence leak.
///
/// This function is the pure encoding of that injection. It:
/// 1. Re-runs [`authorize_openapi`] (double-check — a query built from a
///    non-service-account principal is rejected before the filter is built).
/// 2. Requires the principal to carry a `project_id`. Service-account keys
///    always have one in production (`auth.go:146`), so a missing project is a
///    misconfiguration we refuse to silently widen.
/// 3. Returns a [`FilteredQuery`] tagged with the project and principal.
pub fn apply_project_filter(
    raw_query: impl Into<String>,
    principal: &Principal,
) -> Result<FilteredQuery, ConduitError> {
    authorize_openapi(principal)?;

    let project_id = principal.project_id.as_deref().ok_or_else(|| {
        ConduitError::new(
            ErrorKind::Forbidden,
            "openapi graphql principal lacks a project_id; refusing unscoped read",
        )
    })?;

    Ok(FilteredQuery {
        raw_query: raw_query.into(),
        project_id: project_id.to_string(),
        authorised_by: principal.clone(),
    })
}

// =============================================================================
// Legacy back-compat wrappers (pre-Pasteur-the-3rd API)
// =============================================================================
//
// Earlier iterations of this crate (Bacon / Pasteur) exposed ad-hoc
// `OpenApiApiKeyType` / `OpenApiApiKeyGuardInput` types. They are retained as
// thin shims over the canonical [`authorize_openapi`] path so any caller that
// hasn't been migrated keeps compiling. New code should call
// [`authorize_openapi`] / [`apply_project_filter`] directly.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenApiApiKeyType {
    ServiceAccount,
    User,
}

impl OpenApiApiKeyType {
    pub const fn to_auth_kind(self) -> ApiKeyKind {
        match self {
            Self::ServiceAccount => ApiKeyKind::ServiceAccount,
            Self::User => ApiKeyKind::User,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiApiKeyGuardInput {
    pub api_key_id: String,
    pub key_type: OpenApiApiKeyType,
    pub project_id: String,
}

impl OpenApiApiKeyGuardInput {
    fn to_principal(&self) -> Principal {
        Principal::api_key(self.api_key_id.clone(), self.project_id.clone())
            .with_api_key_kind(self.key_type.to_auth_kind())
    }
}

/// Legacy A01 entry point. Prefer [`authorize_openapi`] (which takes a real
/// `conduit-auth::Principal`).
pub fn require_service_account_api_key(
    input: &OpenApiApiKeyGuardInput,
) -> Result<(), ConduitError> {
    authorize_openapi(&input.to_principal())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiGraphqlContext {
    pub api_key_id: String,
    pub project_id: String,
    pub service_account: bool,
}

impl OpenApiGraphqlContext {
    pub fn new(
        api_key_id: impl Into<String>,
        project_id: impl Into<String>,
        service_account: bool,
    ) -> Self {
        Self {
            api_key_id: api_key_id.into(),
            project_id: project_id.into(),
            service_account,
        }
    }

    fn to_principal(&self) -> Principal {
        let kind = if self.service_account {
            ApiKeyKind::ServiceAccount
        } else {
            ApiKeyKind::User
        };
        Principal::api_key(self.api_key_id.clone(), self.project_id.clone()).with_api_key_kind(kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiProjectScopeGuard {
    pub api_key_id: String,
    pub project_id: String,
}

/// Legacy project-scope guard. Prefer [`apply_project_filter`].
pub fn ensure_project_scope(
    ctx: &OpenApiGraphqlContext,
    project_id: &str,
) -> Result<OpenApiProjectScopeGuard, ConduitError> {
    let principal = ctx.to_principal();
    authorize_openapi(&principal)?;

    if ctx.project_id != project_id {
        return Err(ConduitError::forbidden(
            "openapi graphql api key cannot access the requested project",
        ));
    }

    Ok(OpenApiProjectScopeGuard {
        api_key_id: ctx.api_key_id.clone(),
        project_id: ctx.project_id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // A01 — authorize_openapi
    // -------------------------------------------------------------------------

    #[test]
    fn authorize_openapi_accepts_service_account_api_key() {
        let principal = Principal::api_key_service_account("svc-1", "project-1");
        let outcome = authorize_openapi(&principal);
        assert!(outcome.is_ok(), "service-account key must be authorised");
    }

    #[test]
    fn authorize_openapi_rejects_user_api_key_with_403() {
        // A01: 非 service-account API key 403.
        let principal = Principal::api_key("user-key-1", "project-1");
        match authorize_openapi(&principal) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert_eq!(err.http_status, 403);
            }
            Ok(_) => panic!("user key must be rejected"),
        }
    }

    #[test]
    fn authorize_openapi_rejects_noauth_api_key_with_403() {
        // Go `WithOpenAPIAuth` never accepts the noauth fallback key (it would
        // have been filtered by the outer WithAPIKeyAuth chain); we double-check
        // here as defence-in-depth.
        let principal = Principal::noauth_api_key("project-1");
        match authorize_openapi(&principal) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert_eq!(err.http_status, 403);
            }
            Ok(_) => panic!("noauth key must be rejected"),
        }
    }

    #[test]
    fn authorize_openapi_rejects_user_principal_with_403() {
        // A JWT-authenticated user principal reaches the admin surface, not the
        // OpenAPI surface.
        let principal = Principal::user("user-1");
        match authorize_openapi(&principal) {
            Err(err) => assert_eq!(err.kind, ErrorKind::Forbidden),
            Ok(_) => panic!("user principal must be rejected"),
        }
    }

    #[test]
    fn authorize_openapi_rejects_system_principal_with_403() {
        // System/test bypass principals power the admin tests; the OpenAPI
        // surface must still require a real service-account key, so we refuse
        // them defensively even though they would otherwise bypass RBAC.
        match authorize_openapi(&Principal::system()) {
            Err(err) => assert_eq!(err.kind, ErrorKind::Forbidden),
            Ok(_) => panic!("system principal must be rejected"),
        }
    }

    #[test]
    fn authorize_openapi_rejects_test_principal_with_403() {
        match authorize_openapi(&Principal::test()) {
            Err(err) => assert_eq!(err.kind, ErrorKind::Forbidden),
            Ok(_) => panic!("test principal must be rejected"),
        }
    }

    // -------------------------------------------------------------------------
    // A02 — apply_project_filter
    // -------------------------------------------------------------------------

    #[test]
    fn apply_project_filter_tags_query_with_principal_project() {
        let principal = Principal::api_key_service_account("svc-1", "project-1");
        match apply_project_filter("query { apiKey { id } }", &principal) {
            Ok(filtered) => {
                assert_eq!(filtered.raw_query, "query { apiKey { id } }");
                assert_eq!(filtered.project_id, "project-1");
                assert_eq!(
                    filtered.authorised_by.project_id.as_deref(),
                    Some("project-1")
                );
                assert!(filtered.authorised_by.api_key_kind.is_service_account());
            }
            Err(err) => panic!("service-account filter must succeed: {err:?}"),
        }
    }

    #[test]
    fn apply_project_filter_rejects_non_service_account() {
        // Double-check: apply_project_filter re-runs authorize_openapi, so
        // even if a non-service-account principal somehow reaches the resolver
        // the filter is never built.
        let principal = Principal::api_key("user-key", "project-1");
        match apply_project_filter("query { apiKey { id } }", &principal) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert_eq!(err.http_status, 403);
            }
            Ok(_) => panic!("non-service-account must be rejected before filtering"),
        }
    }

    #[test]
    fn apply_project_filter_rejects_principal_without_project_id() {
        // A service-account key with no project edge is a misconfiguration; we
        // refuse to widen the scope to "all projects".
        let mut principal = Principal::api_key_service_account("svc-1", "project-1");
        principal.project_id = None;
        match apply_project_filter("query { apiKey { id } }", &principal) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert!(err.message.contains("project_id"));
            }
            Ok(_) => panic!("missing project_id must be rejected"),
        }
    }

    // -------------------------------------------------------------------------
    // Legacy back-compat wrappers — kept compiling
    // -------------------------------------------------------------------------

    fn guard_input(key_type: OpenApiApiKeyType) -> OpenApiApiKeyGuardInput {
        OpenApiApiKeyGuardInput {
            api_key_id: "key-1".to_string(),
            key_type,
            project_id: "project-1".to_string(),
        }
    }

    fn context(service_account: bool, project_id: &str) -> OpenApiGraphqlContext {
        OpenApiGraphqlContext::new("key-1", project_id, service_account)
    }

    #[test]
    fn legacy_service_account_api_key_is_allowed() {
        let res = require_service_account_api_key(&guard_input(OpenApiApiKeyType::ServiceAccount));
        assert!(
            res.is_ok(),
            "legacy service-account path must still authorise"
        );
    }

    #[test]
    fn legacy_user_api_key_is_forbidden() {
        let res = require_service_account_api_key(&guard_input(OpenApiApiKeyType::User));
        match res {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert_eq!(err.http_status, 403);
            }
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn legacy_service_account_project_scope_is_allowed() {
        match ensure_project_scope(&context(true, "project-1"), "project-1") {
            Ok(guard) => {
                assert_eq!(guard.api_key_id, "key-1");
                assert_eq!(guard.project_id, "project-1");
            }
            Err(err) => panic!("same-project service-account must be allowed: {err:?}"),
        }
    }

    #[test]
    fn legacy_non_service_account_project_scope_is_forbidden() {
        match ensure_project_scope(&context(false, "project-1"), "project-1") {
            Err(err) => assert_eq!(err.kind, ErrorKind::Forbidden),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn legacy_project_mismatch_is_forbidden() {
        match ensure_project_scope(&context(true, "project-1"), "project-2") {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::Forbidden);
                assert!(err.message.contains("cannot access the requested project"));
            }
            Ok(_) => panic!("expected error"),
        }
    }
}

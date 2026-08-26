//! RUST-P11-003 — Admin GraphQL endpoint handlers.
//!
//! Mounts the `async-graphql` admin schema at `/admin/graphql` (POST) and
//! `/admin/playground` (GET), mirroring the Go routes at
//! `conduit/internal/server/routes.go:99-104`:
//!
//! ```text
//! adminGroup.GET("/playground", ...) { handlers.Graphql.Playground.ServeHTTP(...) }
//! adminGroup.POST("/graphql", ...)   { handlers.Graphql.Graphql.ServeHTTP(...) }
//! ```
//!
//! Both routes live inside the JWT-authenticated `/admin` group, so the
//! `jwt_admin_auth` middleware runs before these handlers are reached.
//!
//! ## Design
//!
//! - The [`AdminSchema`] (from `conduit-admin-graphql`) is stored in [`AppState`]
//!   and extracted via axum's `State` extractor.
//! - `POST /admin/graphql` uses `async_graphql_axum::GraphQL` handler for
//!   standard request execution. Introspection is enabled by default (the
//!   frontend's Apollo/urql tooling requires it).
//! - `GET /admin/playground` returns the GraphQL Playground HTML page, pointing
//!   at the `/admin/graphql` endpoint. This mirrors Go's
//!   `playground.Handler("GraphQL Playground", "/admin/graphql")` call.

use async_graphql::http::{GraphQLPlaygroundConfig, playground_source};
use axum::Extension;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Response, StatusCode, header};
use axum::response::IntoResponse;
use conduit_admin_graphql::AdminSchema;
use conduit_admin_graphql::me::CurrentUser;
use conduit_auth::request_context::ApiKeyContext;
use conduit_auth::scopes::slug;
use conduit_auth::{Principal, RequestContext, RequestSource};

use crate::app_state::AppState;
use crate::middleware::AuthRequestContextExtension;
use crate::middleware::api_key_auth::ValidatedApiKeyMetadata;

/// `POST /admin/graphql` — execute a GraphQL query/mutation against the admin
/// schema.
///
/// Uses `async_graphql_axum::GraphQLRequest` to parse the incoming JSON body
/// (standard `{ "query": "...", "variables": {...}, "operationName": "..." }`
/// shape) and executes it against the schema stored in state.
///
/// The response is a standard GraphQL JSON response:
/// `{ "data": {...}, "errors": [...] }` — matching what `async-graphql`
/// produces and what the frontend expects from the Go gqlgen handler.
///
/// ## Per-request user propagation
///
/// The admin schema is built once at boot (a singleton), so a wired
/// `MeServices` adapter cannot know *which* user issued a given request. Go
/// resolves the current user from the gin request context
/// (`contexts.GetUser(ctx)`), which the `WithJWTAuth` middleware populated. The
/// Rust JWT middleware (`middleware::jwt_auth::jwt_admin_auth`) mirrors that by
/// injecting an [`AuthRequestContextExtension`] carrying the decoded `user_id`.
///
/// Here we lift that `user_id` off the axum request extension and push it into
/// the per-request async-graphql data bag as a [`CurrentUser`]. The `me` /
/// `myProjects` resolvers read it back (`me::current_user`) and forward it to
/// the service — the Rust equivalent of Go's `GetUserByID(ctx, userCtx.ID)`.
/// When no auth extension is present (e.g. an unauthenticated smoke request),
/// no `CurrentUser` is injected and the resolvers surface Go's
/// "user not found in context" error.
pub async fn graphql_handler(
    Extension(schema): Extension<AdminSchema>,
    // Optional: the admin group is JWT-guarded so this is normally present, but
    // the extractor stays optional so a mis-wired route can never 500 here.
    auth: Option<Extension<AuthRequestContextExtension>>,
    req: async_graphql_axum::GraphQLRequest,
) -> axum::response::Response {
    let mut request = req.into_inner();

    // Lift the JWT-decoded user id (if any) into the per-request data bag.
    if let Some(Extension(auth)) = auth {
        if let Some(user) = auth.context().user.as_ref() {
            request = request.data(CurrentUser {
                user_id: user.user_id,
            });
        }
        // Also publish the whole auth `RequestContext` (principal + project_id).
        //
        // Go's resolvers reach the principal through the request context, which
        // the ent privacy layer then consults on every query
        // (`scopes.Policy` is default-deny, `scopes/policy.go:36-52`). The Rust
        // admin schema is a boot singleton, so the per-request identity has to
        // travel through the async-graphql data bag; publishing the context here
        // is what lets a resolver-level guard authorize against the *real*
        // principal instead of a fabricated one.
        //
        // The JWT middleware enriches this principal with the user's
        // `is_owner` + scope slugs (see `middleware::enrich_jwt_context`), so it
        // is usable by `conduit_auth::rbac` as-is.
        request = request.data(auth.into_context());
    }

    let response = schema.execute(request).await;
    frontend_compatible_graphql_response(response)
}

/// `POST /internal/v1/graphql` — automation access to the full administrator
/// schema. This is deliberately separate from both the browser/JWT admin
/// endpoint and the project-scoped OpenAPI schema.
///
/// Only a DB-validated `service_account` key carrying the dedicated
/// `system:admin` scope is accepted. Ordinary service accounts remain confined
/// to `/openapi/v1/graphql`; the dedicated scope is promoted to owner authority
/// only for this endpoint so owner-only billing operations can be automated.
pub async fn internal_graphql_handler(
    Extension(schema): Extension<AdminSchema>,
    metadata: Option<Extension<ValidatedApiKeyMetadata>>,
    req: async_graphql_axum::GraphQLRequest,
) -> axum::response::Response {
    let Some(Extension(metadata)) = metadata else {
        return crate::api_error::json_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    };
    if metadata.key_type != "service_account" {
        return crate::api_error::json_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    }
    if !metadata
        .scopes
        .iter()
        .any(|scope| scope == slug::SYSTEM_ADMIN)
    {
        return crate::api_error::json_error(
            StatusCode::FORBIDDEN,
            "The service account requires the system:admin scope",
        );
    }

    let mut principal = Principal::api_key_service_account(
        metadata.api_key_id.to_string(),
        metadata.project_id.to_string(),
    )
    .with_owner(true);
    for scope in &metadata.scopes {
        principal = principal.with_scope(scope.clone());
    }
    let request_context = RequestContext {
        principal: Some(principal),
        // This endpoint is intentionally system-wide. Individual resolver
        // inputs still select their target user/project explicitly.
        project_id: None,
        source: Some(RequestSource::Internal),
        api_key: Some(ApiKeyContext {
            api_key_id: metadata.api_key_id,
            project_id: metadata.project_id,
            name: Some(metadata.api_key_name),
            api_key_type: Some(metadata.key_type),
            status: Some("enabled".to_string()),
            ..ApiKeyContext::default()
        }),
        ..RequestContext::default()
    };

    let response = schema.execute(req.into_inner().data(request_context)).await;
    frontend_compatible_graphql_response(response)
}

/// The retained frontend predates the GraphQL-over-HTTP media type and accepts
/// only `application/json`. `async-graphql-axum` now defaults successful
/// responses to `application/graphql-response+json`; although standards
/// compliant, that makes the frontend reject a valid response before parsing
/// it. Keep this compatibility decision at the HTTP boundary.
fn frontend_compatible_graphql_response(
    response: async_graphql::Response,
) -> axum::response::Response {
    let mut response = async_graphql_axum::GraphQLResponse::from(response).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

/// `GET /admin/playground` — serve the GraphQL Playground HTML page.
///
/// Mirrors Go `playground.Handler("GraphQL Playground", "/admin/graphql")`
/// from `routes.go:99-100`. The playground UI is a self-contained HTML page
/// that issues requests to the configured endpoint.
///
/// The endpoint path respects `server.base_path`: when the base path is
/// non-empty (e.g. `/gateway`), the playground points at
/// `/gateway/admin/graphql`.
pub async fn graphql_playground(State(state): State<AppState>) -> impl IntoResponse {
    let base_path = state.base_path();
    let endpoint = if base_path.is_empty() || base_path == "/" {
        "/admin/graphql".to_string()
    } else {
        format!("{}/admin/graphql", base_path.trim_end_matches('/'))
    };

    let html = playground_source(GraphQLPlaygroundConfig::new(&endpoint));

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("failed to build playground response"))
                .unwrap_or_default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_graphql_response_uses_frontend_compatible_content_type() {
        let response = frontend_compatible_graphql_response(async_graphql::Response::new(
            async_graphql::Value::Null,
        ));

        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
    }

    #[test]
    fn internal_admin_scope_is_distinct_from_ordinary_service_account_scopes() {
        let ordinary = ValidatedApiKeyMetadata {
            key_type: "service_account".to_string(),
            scopes: vec!["read_users".to_string(), "write_users".to_string()],
            ..ValidatedApiKeyMetadata::default()
        };
        assert!(
            !ordinary
                .scopes
                .iter()
                .any(|scope| scope == slug::SYSTEM_ADMIN)
        );

        let internal = ValidatedApiKeyMetadata {
            scopes: vec![slug::SYSTEM_ADMIN.to_string()],
            ..ordinary
        };
        assert!(
            internal
                .scopes
                .iter()
                .any(|scope| scope == slug::SYSTEM_ADMIN)
        );
    }
}

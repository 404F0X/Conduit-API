use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conduit_auth::Principal;

use crate::AppState;
use crate::api_error::json_error;
use crate::middleware::api_key_auth::ValidatedApiKeyMetadata;

pub async fn graphql_handler(
    State(state): State<AppState>,
    metadata: Option<Extension<ValidatedApiKeyMetadata>>,
    request: async_graphql_axum::GraphQLRequest,
) -> Response {
    let Some(Extension(metadata)) = metadata else {
        return json_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    };
    if metadata.key_type != "service_account" {
        return json_error(StatusCode::UNAUTHORIZED, "Invalid API key");
    }
    let Some(schema) = state.services().openapi_schema() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "OpenAPI GraphQL service is unavailable",
        );
    };

    let mut principal = Principal::api_key_service_account(
        metadata.api_key_id.to_string(),
        metadata.project_id.to_string(),
    );
    for scope in metadata.scopes {
        principal = principal.with_scope(scope);
    }
    let response = schema.execute(request.into_inner().data(principal)).await;
    async_graphql_axum::GraphQLResponse::from(response).into_response()
}

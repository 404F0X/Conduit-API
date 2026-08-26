#![forbid(unsafe_code)]

//! OpenAPI GraphQL surface (RUST-P12-002 / MAP-01).
//!
//! Full port of Go `conduit/internal/server/gql/openapi/*` against the frozen
//! contract snapshot `tests/contracts/openapi_graphql_schema.graphql`:
//!
//! * [`model`] — every SDL type/enum/input, name-for-name;
//! * [`resolver`] — the `Query`/`Mutation` roots (`apiKey`,
//!   `apiKeyQuotaUsages`, `createLLMAPIKey`, `updateAPIKeyProfiles`,
//!   `loadApiKeyProfileTemplate`), semantics mirrored from
//!   `openapi.resolvers.go` + `helper.go`;
//! * [`service`] — trait seams standing in for `biz.APIKeyService`,
//!   `biz.APIKeyProfileTemplateService`, `biz.QuotaService`;
//! * [`scalars`] — `Time` / `Decimal` / `DecimalInput` wire shims and the
//!   `gid://conduit/<Type>/<id>` GUID codec;
//! * [`guard`] — the pure A01/A02 predicates ([`authorize_openapi`],
//!   [`apply_project_filter`]);
//! * [`contract`] — a structural SDL parser + the snapshot diff used by the
//!   contract tests.
//!
//! # HTTP wiring (owned by conduit-http, P11/P2 — intentionally NOT here)
//!
//! The schema built by [`build_openapi_schema`] is expected to be mounted at
//! **`POST /openapi/v1/graphql`** (Go `routes.go:145` +
//! `openapi/graphql.go:59`). Go deliberately registers no GET transport: the
//! `apiKeyQuotaUsages` query accepts a plaintext `key`, and GET would let
//! secret keys ride URLs into proxy logs (`graphql.go:37-44`,
//! `TestOpenAPIHandler_RejectsGET`). The HTTP layer must:
//!
//! 1. authenticate the bearer API key, reject non-service-account keys
//!    ([`authorize_openapi`]; Go's `WithOpenAPIAuth` replies 401 "Invalid API
//!    key" there — the A01 task contract labels the post-authentication
//!    decision 403, which is what the pure guard returns);
//! 2. inject the resulting [`conduit_auth::Principal`] (scopes + project) as
//!    per-request data (`Request::data`), which is how the resolvers and the
//!    service layer receive it.

pub mod contract;
pub mod guard;
#[cfg(test)]
pub(crate) mod memory;
pub mod model;
pub mod resolver;
pub mod scalars;
pub mod service;

use async_graphql::{EmptySubscription, Schema};

pub use contract::{
    OPENAPI_SDL_ADMIN_ONLY_FIELDS, OPENAPI_SDL_EXPECTED_ENUM_TYPES,
    OPENAPI_SDL_EXPECTED_INPUT_TYPES, OPENAPI_SDL_EXPECTED_MUTATION_FIELDS,
    OPENAPI_SDL_EXPECTED_OBJECT_TYPES, OPENAPI_SDL_EXPECTED_QUERY_FIELDS,
    OPENAPI_SDL_EXPECTED_TYPES, SdlField, SdlIndex, SdlType, diff_sdl, normalize_description,
    parse_sdl,
};
pub use guard::{
    FilteredQuery, OpenApiApiKeyGuardInput, OpenApiApiKeyType, OpenApiGraphqlContext,
    OpenApiProjectScopeGuard, apply_project_filter, authorize_openapi, ensure_project_scope,
    require_service_account_api_key,
};
pub use model::{
    APIKey, APIKeyProfile, APIKeyProfileInput, APIKeyProfileQuotaUsage, APIKeyProfiles,
    APIKeyQuota, APIKeyQuotaCalendarDuration, APIKeyQuotaCalendarDurationUnit, APIKeyQuotaInput,
    APIKeyQuotaPastDuration, APIKeyQuotaPastDurationUnit, APIKeyQuotaPeriod, APIKeyQuotaPeriodType,
    APIKeyQuotaUsage, APIKeyQuotaWindow, ChannelTagsMatchMode, LoadApiKeyProfileTemplateInput,
    ModelMapping, UpdateAPIKeyProfilesInput,
};
pub use resolver::{Mutation, Query};
pub use scalars::{
    GUID_PREFIX, GUID_TYPE_API_KEY, GUID_TYPE_API_KEY_PROFILE_TEMPLATE, GqlDecimal,
    GqlDecimalInput, GqlTime, OpenApiGuid, validate_guid_type,
};
pub use service::{
    ApiKeyProfileTemplateRecord, ApiKeyRecord, OpenApiApiKeyProfileTemplateService,
    OpenApiApiKeyService, OpenApiQuotaService, OpenApiServices, ProfileQuotaUsage, QuotaUsage,
    QuotaWindow,
};

pub const CRATE_NAME: &str = "conduit-openapi-graphql";

/// The executable OpenAPI schema type — the Rust analogue of Go `NewSchema`'s
/// `graphql.ExecutableSchema` (`openapi/resolver.go:20-32`).
pub type OpenApiSchema = Schema<Query, Mutation, EmptySubscription>;

/// Build the OpenAPI schema over the given service bundle.
///
/// Mirrors Go `NewSchema(apiKeyService, apiKeyProfileTemplateService,
/// quotaService)`: the services are installed as schema data so every
/// resolver can reach them; the per-request [`conduit_auth::Principal`] is
/// supplied by the HTTP layer via `Request::data` (the analogue of
/// `WithOpenAPIAuth` populating the request context).
pub fn build_openapi_schema(services: OpenApiServices) -> OpenApiSchema {
    Schema::build(Query, Mutation, EmptySubscription)
        .data(services)
        .finish()
}

//! OpenAPI GraphQL Query/Mutation roots.
//!
//! Verbatim port of `internal/server/gql/openapi/openapi.resolvers.go` +
//! `helper.go`. Every root field name, argument set and description mirrors the
//! frozen snapshot (`tests/contracts/openapi_graphql_schema.graphql`); the
//! contract test in [`crate::contract`] enforces it.
//!
//! Naming gotchas: `createLLMAPIKey` / `updateAPIKeyProfiles` need explicit
//! `#[graphql(name = ...)]` (the default camelCase rename would emit
//! `createLlmApiKey` / `updateApiKeyProfiles`); `loadApiKeyProfileTemplate`,
//! `apiKey`, `apiKeyQuotaUsages` and the `apiKeyId` argument are produced
//! correctly by the default rename.
//!
//! Authorization split (A01/A02):
//! * A01 — "only service-account keys may use this surface" is enforced BEFORE
//!   the schema executes, by the HTTP middleware (Go `WithOpenAPIAuth`,
//!   `middleware/auth.go:140-143`). The pure predicate lives in
//!   [`crate::guard::authorize_openapi`]; the conduit-http wiring (P11/P2)
//!   calls it and maps the failure to a status code.
//! * A02 — per-project visibility is NOT re-derived here: exactly like the Go
//!   resolvers lean on the ent privacy layer, these resolvers pass the caller
//!   [`Principal`] to the [`crate::service`] traits, whose implementations
//!   enforce scope + own-project filtering (uniform NotFound for foreign
//!   rows).

use async_graphql::{Context, ID, Object};
use conduit_auth::Principal;
use conduit_core::ConduitError;

use crate::model::{
    APIKey, APIKeyProfileQuotaUsage, APIKeyQuotaUsage, APIKeyQuotaWindow,
    LoadApiKeyProfileTemplateInput, UpdateAPIKeyProfilesInput,
};
use crate::scalars::{
    GUID_TYPE_API_KEY, GUID_TYPE_API_KEY_PROFILE_TEMPLATE, GqlDecimal, GqlTime, OpenApiGuid,
    validate_guid_type,
};
use crate::service::{ApiKeyProfileTemplateRecord, ApiKeyRecord, OpenApiServices};

/// Root query object — renders as `type Query` and carries exactly the two
/// fields of the snapshot's `extend type Query` block.
pub struct Query;

/// Root mutation object — renders as `type Mutation` and carries exactly the
/// three fields of the snapshot's `extend type Mutation` block.
pub struct Mutation;

// ---------------------------------------------------------------------------
// Shared resolver plumbing (mirrors helper.go + the ctx lookups).
// ---------------------------------------------------------------------------

// Map a domain error onto the GraphQL error surface. gqlgen renders
// `err.Error()` as the error message with no extensions for these paths, so
// only the message crosses over.
fn to_gql_error(err: ConduitError) -> async_graphql::Error {
    async_graphql::Error::new(err.message)
}

// The caller principal injected per-request by the HTTP layer (the analogue of
// Go `contexts.GetAPIKey`). Missing principal mirrors the Go resolver error
// string (`openapi.resolvers.go:18-21`).
fn principal_from_ctx<'a>(ctx: &Context<'a>) -> Result<&'a Principal, async_graphql::Error> {
    ctx.data_opt::<Principal>()
        .ok_or_else(|| async_graphql::Error::new("api key not found in context"))
}

// The service bundle installed as schema data by `build_openapi_schema`.
fn services_from_ctx<'a>(ctx: &Context<'a>) -> Result<&'a OpenApiServices, async_graphql::Error> {
    ctx.data::<OpenApiServices>()
}

// Parse + type-check an optional GUID argument. The parse step mirrors Go's
// `objects.GUID.UnmarshalGQL` (which gqlgen runs during input coercion); the
// type check mirrors `guidID` (`helper.go:40-50`). Both run before any lookup.
fn guid_id(id: Option<&ID>, expected_type: &str) -> Result<Option<i64>, ConduitError> {
    match id {
        None => Ok(None),
        Some(raw) => {
            let guid = OpenApiGuid::parse(raw.as_str())?;
            validate_guid_type(Some(&guid), expected_type)
        }
    }
}

// Mirrors `resolveAPIKey` (`helper.go:16-23`): typed-GUID validation, then the
// privacy-gated read path (exactly-one-of + read_api_keys + own-project).
async fn resolve_api_key(
    services: &OpenApiServices,
    caller: &Principal,
    id: Option<&ID>,
    key: Option<&str>,
    name: Option<&str>,
) -> Result<ApiKeyRecord, ConduitError> {
    let key_id = guid_id(id, GUID_TYPE_API_KEY)?;
    services
        .api_keys
        .get_for_read(caller, key_id, key, name)
        .await
}

// Mirrors `resolveTemplate` (`helper.go:26-33`).
async fn resolve_template(
    services: &OpenApiServices,
    caller: &Principal,
    id: Option<&ID>,
    name: Option<&str>,
) -> Result<ApiKeyProfileTemplateRecord, ConduitError> {
    let template_id = guid_id(id, GUID_TYPE_API_KEY_PROFILE_TEMPLATE)?;
    services
        .templates
        .get_for_read(caller, template_id, name)
        .await
}

// Mirrors `toOpenAPIAPIKey` (`helper.go:57-69`): project the rich record down
// to the minimal OpenAPI surface, formatting the GUID exactly like Go's
// `objects.GUID{Type: "APIKey", ID: k.ID}.MarshalGQL`.
fn to_api_key_object(record: ApiKeyRecord) -> APIKey {
    APIKey {
        id: ID::from(OpenApiGuid::new(GUID_TYPE_API_KEY, record.id).to_string()),
        key: record.key,
        name: record.name,
        scopes: record.scopes,
        profiles: record.profiles,
    }
}

// ---------------------------------------------------------------------------
// Query — mirrors queryResolver (openapi.resolvers.go:84-124).
// ---------------------------------------------------------------------------

#[Object]
impl Query {
    /// Returns an API key's details (id, key, name, scopes, profiles).
    /// Provide exactly one of id, key, or name; all identify a key, and only keys
    /// inside the caller's own project are visible (requires the read_api_keys scope).
    async fn api_key(
        &self,
        ctx: &Context<'_>,
        id: Option<ID>,
        key: Option<String>,
        name: Option<String>,
    ) -> async_graphql::Result<APIKey> {
        let services = services_from_ctx(ctx)?;
        let caller = principal_from_ctx(ctx)?;

        let record = resolve_api_key(
            services,
            caller,
            id.as_ref(),
            key.as_deref(),
            name.as_deref(),
        )
        .await
        .map_err(to_gql_error)?;

        Ok(to_api_key_object(record))
    }

    /// Returns quota usage for the profiles that have quota enabled on an API key.
    /// Provide exactly one of apiKeyId, key, or name; all identify a key, and only
    /// keys inside the caller's own project are visible (requires the read_api_keys
    /// scope).
    async fn api_key_quota_usages(
        &self,
        ctx: &Context<'_>,
        api_key_id: Option<ID>,
        key: Option<String>,
        name: Option<String>,
    ) -> async_graphql::Result<Vec<APIKeyProfileQuotaUsage>> {
        let services = services_from_ctx(ctx)?;
        let caller = principal_from_ctx(ctx)?;

        let api_key = resolve_api_key(
            services,
            caller,
            api_key_id.as_ref(),
            key.as_deref(),
            name.as_deref(),
        )
        .await
        .map_err(to_gql_error)?;

        let usages = services
            .quota
            .profile_quota_usages(caller, &api_key)
            .await
            .map_err(to_gql_error)?;

        // Field-for-field the mapping loop of openapi.resolvers.go:106-121.
        let out = usages
            .into_iter()
            .map(|u| APIKeyProfileQuotaUsage {
                profile_name: u.profile_name,
                quota: u.quota,
                window: APIKeyQuotaWindow {
                    start: u.window.start.map(GqlTime),
                    end: u.window.end.map(GqlTime),
                },
                usage: APIKeyQuotaUsage {
                    request_count: u.usage.request_count,
                    total_tokens: u.usage.total_tokens,
                    total_cost: GqlDecimal(u.usage.total_cost),
                },
            })
            .collect();

        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Mutation — mirrors mutationResolver (openapi.resolvers.go:16-82).
// ---------------------------------------------------------------------------

#[Object]
impl Mutation {
    // No description in the snapshot — keep this comment a plain `//`.
    // Mirrors CreateLLMAPIKey (openapi.resolvers.go:17-29): requires the owner
    // API key in context, delegates to the api-key service.
    #[graphql(name = "createLLMAPIKey")]
    async fn create_llm_api_key(
        &self,
        ctx: &Context<'_>,
        name: String,
    ) -> async_graphql::Result<APIKey> {
        let services = services_from_ctx(ctx)?;
        let caller = principal_from_ctx(ctx)?;

        let record = services
            .api_keys
            .create_llm_api_key(caller, &name)
            .await
            .map_err(to_gql_error)?;

        Ok(to_api_key_object(record))
    }

    /// Updates the profiles of an API key. Provide exactly one of id or name;
    /// name resolves within the caller's own project, where API key names are unique.
    #[graphql(name = "updateAPIKeyProfiles")]
    async fn update_api_key_profiles(
        &self,
        ctx: &Context<'_>,
        id: Option<ID>,
        name: Option<String>,
        mut input: UpdateAPIKeyProfilesInput,
    ) -> async_graphql::Result<APIKey> {
        let services = services_from_ctx(ctx)?;
        let caller = principal_from_ctx(ctx)?;

        // Coerce nil ModelMappings to [] before saving — verbatim from the Go
        // resolver (openapi.resolvers.go:33-42), see the model helper's docs.
        input.normalize_model_mappings();

        // Resolve the target through the privacy-gated read path so a name
        // identifies a key as reliably as an id (openapi.resolvers.go:44-51).
        let target = resolve_api_key(services, caller, id.as_ref(), None, name.as_deref())
            .await
            .map_err(to_gql_error)?;

        let record = services
            .api_keys
            .update_api_key_profiles(caller, target.id, input.into_profiles())
            .await
            .map_err(to_gql_error)?;

        Ok(to_api_key_object(record))
    }

    // No description in the snapshot (the documentation lives on the input's
    // fields). Mirrors LoadAPIKeyProfileTemplate (openapi.resolvers.go:62-82):
    // resolve template first, then the target key, then load.
    async fn load_api_key_profile_template(
        &self,
        ctx: &Context<'_>,
        input: LoadApiKeyProfileTemplateInput,
    ) -> async_graphql::Result<APIKey> {
        let services = services_from_ctx(ctx)?;
        let caller = principal_from_ctx(ctx)?;

        let template = resolve_template(
            services,
            caller,
            input.template_id.as_ref(),
            input.template_name.as_deref(),
        )
        .await
        .map_err(to_gql_error)?;

        let target = resolve_api_key(
            services,
            caller,
            input.api_key_id.as_ref(),
            None,
            input.api_key_name.as_deref(),
        )
        .await
        .map_err(to_gql_error)?;

        let record = services
            .templates
            .load_template(caller, template.id, target.id)
            .await
            .map_err(to_gql_error)?;

        Ok(to_api_key_object(record))
    }
}

// ---------------------------------------------------------------------------
// Resolver tests — mirror internal/server/gql/openapi/openapi_test.go
// case-for-case, executed through the real schema (arg coercion included).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use async_graphql::{Request, Variables};
    use conduit_auth::scopes::slug;
    use serde_json::{Value, json};

    use crate::memory::{SeededUsage, TestEnv, fixture};
    use crate::{OpenApiSchema, build_openapi_schema};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const API_KEY_QUERY: &str = "query($id: ID, $key: String, $name: String) {\n\
         apiKey(id: $id, key: $key, name: $name) {\n\
           id key name scopes\n\
           profiles { activeProfile profiles { name modelMappings { from to } } }\n\
         }\n\
       }";

    // Mirrors the Go e2e `quotaQuery` shape (openapi_e2e_test.go:47-53).
    const QUOTA_QUERY: &str = "query($id: ID, $key: String, $name: String) {\n\
         apiKeyQuotaUsages(apiKeyId: $id, key: $key, name: $name) {\n\
           profileName\n\
           quota { requests totalTokens cost period { type } }\n\
           window { start end }\n\
           usage { requestCount totalTokens totalCost }\n\
         }\n\
       }";

    const CREATE_MUTATION: &str = "mutation($name: String!) {\n\
         createLLMAPIKey(name: $name) { id key name scopes }\n\
       }";

    const UPDATE_MUTATION: &str = "mutation($id: ID, $name: String, $input: UpdateAPIKeyProfilesInput!) {\n\
         updateAPIKeyProfiles(id: $id, name: $name, input: $input) {\n\
           id key name\n\
           profiles { activeProfile profiles { name modelMappings { from to } } }\n\
         }\n\
       }";

    const LOAD_MUTATION: &str = "mutation($input: LoadApiKeyProfileTemplateInput!) {\n\
         loadApiKeyProfileTemplate(input: $input) {\n\
           id name\n\
           profiles { activeProfile profiles { name modelMappings { from to } } }\n\
         }\n\
       }";

    fn setup(scopes: &[&str]) -> (OpenApiSchema, TestEnv) {
        let env = fixture(scopes);
        (build_openapi_schema(env.services.clone()), env)
    }

    async fn exec(
        schema: &OpenApiSchema,
        env: &TestEnv,
        query: &str,
        vars: Value,
    ) -> async_graphql::Response {
        let request = Request::new(query)
            .variables(Variables::from_json(vars))
            .data(env.principal.clone());
        schema.execute(request).await
    }

    // Successful-response helper: fails the test on any GraphQL error.
    fn data_of(resp: async_graphql::Response) -> Result<Value, Box<dyn std::error::Error>> {
        if !resp.errors.is_empty() {
            return Err(format!("unexpected graphql errors: {:?}", resp.errors).into());
        }
        Ok(resp.data.into_json()?)
    }

    // Error-response helper: returns the first error message.
    fn first_error(resp: async_graphql::Response) -> Result<String, Box<dyn std::error::Error>> {
        resp.errors
            .first()
            .map(|e| e.message.clone())
            .ok_or_else(|| "expected a graphql error, got success".into())
    }

    fn guid(id: i64) -> String {
        format!("gid://conduit/APIKey/{id}")
    }

    fn template_guid(id: i64) -> String {
        format!("gid://conduit/APIKeyProfileTemplate/{id}")
    }

    fn pointer<'a>(value: &'a Value, path: &str) -> Result<&'a Value, Box<dyn std::error::Error>> {
        value
            .pointer(path)
            .ok_or_else(|| format!("missing {path} in {value}").into())
    }

    // -------------------------------------------------------------------
    // createLLMAPIKey — mirrors TestOpenAPIResolver_CreateLLMAPIKey_*.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn create_llm_api_key_happy_path_trims_name_and_assigns_scopes() -> TestResult {
        let (schema, env) = setup(&[slug::WRITE_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            CREATE_MUTATION,
            json!({"name": "  example-key  "}),
        )
        .await;
        let data = data_of(resp)?;

        assert_eq!(
            pointer(&data, "/createLLMAPIKey/name")?,
            &json!("example-key")
        );
        let key = pointer(&data, "/createLLMAPIKey/key")?
            .as_str()
            .ok_or("key must be a string")?;
        assert!(!key.is_empty(), "generated key must be non-empty");

        // ElementsMatch on [read_channels, write_requests].
        let mut scopes: Vec<String> =
            serde_json::from_value(pointer(&data, "/createLLMAPIKey/scopes")?.clone())?;
        scopes.sort();
        assert_eq!(scopes, vec!["read_channels", "write_requests"]);
        Ok(())
    }

    #[tokio::test]
    async fn create_llm_api_key_duplicate_name_rejected() -> TestResult {
        let (schema, env) = setup(&[slug::WRITE_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            CREATE_MUTATION,
            json!({"name": env.fx.target_key_name}),
        )
        .await;
        let message = first_error(resp)?;
        assert!(
            message.contains(&env.fx.target_key_name),
            "duplicate-name error must mention the name, got: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_llm_api_key_missing_scope_denied() -> TestResult {
        // read-only caller (缺 write) — mirrors the Go case.
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            CREATE_MUTATION,
            json!({"name": "should-fail"}),
        )
        .await;
        let message = first_error(resp)?;
        assert!(message.contains("write_api_keys"), "got: {message}");
        Ok(())
    }

    #[tokio::test]
    async fn create_llm_api_key_missing_principal_rejected() -> TestResult {
        // Mirrors the "api key not found in context" branch of the Go resolver
        // (openapi.resolvers.go:18-21): no principal injected at all.
        let (schema, _env) = setup(&[slug::WRITE_API_KEYS]);

        let request =
            Request::new(CREATE_MUTATION).variables(Variables::from_json(json!({"name": "x"})));
        let resp = schema.execute(request).await;
        let message = first_error(resp)?;
        assert_eq!(message, "api key not found in context");
        Ok(())
    }

    // -------------------------------------------------------------------
    // updateAPIKeyProfiles — mirrors TestOpenAPIResolver_UpdateAPIKeyProfiles_*.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn update_api_key_profiles_happy_path() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let input = json!({
            "activeProfile": "Production",
            "profiles": [
                {"name": "Default"},
                {"name": "Production", "modelMappings": [{"from": "gpt-4", "to": "gpt-4o"}]},
            ],
        });
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"id": guid(env.fx.target_key_id), "input": input}),
        )
        .await;
        let data = data_of(resp)?;

        assert_eq!(
            pointer(&data, "/updateAPIKeyProfiles/profiles/activeProfile")?,
            &json!("Production")
        );
        let profiles = pointer(&data, "/updateAPIKeyProfiles/profiles/profiles")?
            .as_array()
            .ok_or("profiles must be a list")?;
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            pointer(&data, "/updateAPIKeyProfiles/profiles/profiles/1/name")?,
            "Production"
        );
        assert_eq!(
            pointer(
                &data,
                "/updateAPIKeyProfiles/profiles/profiles/1/modelMappings/0/from"
            )?,
            "gpt-4"
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_normalizes_nil_model_mappings() -> TestResult {
        // Regression mirror: omitted modelMappings must come back as [] (never
        // null) after the update.
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let input = json!({"activeProfile": "test", "profiles": [{"name": "test"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"id": guid(env.fx.target_key_id), "input": input}),
        )
        .await;
        let data = data_of(resp)?;

        let mappings = pointer(
            &data,
            "/updateAPIKeyProfiles/profiles/profiles/0/modelMappings",
        )?;
        assert_eq!(
            mappings,
            &json!([]),
            "modelMappings must be normalized to a non-null empty list"
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_cross_project_denied() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // 外项目的 key id:privacy 的 read filter 让 Get 找不到 → NotFound。
        let input = json!({"activeProfile": "X", "profiles": [{"name": "X"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"id": guid(env.fx.other_key_id), "input": input}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_missing_write_scope_denied() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]); // 缺 write

        let input = json!({"activeProfile": "Default", "profiles": [{"name": "Default"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"id": guid(env.fx.target_key_id), "input": input}),
        )
        .await;
        assert!(first_error(resp)?.contains("write_api_keys"));
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_by_name() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let input = json!({"activeProfile": "Default", "profiles": [{"name": "Default"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"name": env.fx.target_key_name, "input": input}),
        )
        .await;
        let data = data_of(resp)?;

        assert_eq!(
            pointer(&data, "/updateAPIKeyProfiles/name")?,
            &json!(env.fx.target_key_name)
        );
        // Name must resolve to the same key as its id.
        assert_eq!(
            pointer(&data, "/updateAPIKeyProfiles/id")?,
            &json!(guid(env.fx.target_key_id))
        );
        assert_eq!(
            pointer(&data, "/updateAPIKeyProfiles/profiles/activeProfile")?,
            &json!("Default")
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_requires_exactly_one_identifier() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);
        let input = json!({"activeProfile": "Default", "profiles": [{"name": "Default"}]});

        // Neither provided.
        let resp = exec(&schema, &env, UPDATE_MUTATION, json!({"input": input})).await;
        assert!(first_error(resp)?.contains("exactly one of"));

        // Both provided.
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({
                "id": guid(env.fx.target_key_id),
                "name": env.fx.target_key_name,
                "input": input,
            }),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of"));
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_by_name_cross_project_denied() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // 外项目 key name 在项目过滤下不可见 → NotFound,不泄露存在性。
        let input = json!({"activeProfile": "X", "profiles": [{"name": "X"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({"name": env.fx.other_key_name, "input": input}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    #[tokio::test]
    async fn update_api_key_profiles_invalid_guid_type() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let input = json!({"activeProfile": "Default", "profiles": [{"name": "Default"}]});
        let resp = exec(
            &schema,
            &env,
            UPDATE_MUTATION,
            json!({
                "id": format!("gid://conduit/Channel/{}", env.fx.target_key_id),
                "input": input,
            }),
        )
        .await;
        let message = first_error(resp)?;
        assert!(
            message.contains("expected a APIKey GUID, got Channel"),
            "got: {message}"
        );
        Ok(())
    }

    // -------------------------------------------------------------------
    // loadApiKeyProfileTemplate — mirrors TestOpenAPIResolver_LoadAPIKeyProfileTemplate_*.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn load_template_happy_path_appends_and_keeps_active() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.template_id),
                "apiKeyID": guid(env.fx.target_key_id),
            }}),
        )
        .await;
        let data = data_of(resp)?;

        // Append-only semantics: original Default kept, template appended,
        // active profile unchanged.
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/activeProfile")?,
            &json!("Default")
        );
        let profiles = pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles")?
            .as_array()
            .ok_or("profiles must be a list")?;
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles/0/name")?,
            "Default"
        );
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles/1/name")?,
            "Production"
        );
        Ok(())
    }

    #[tokio::test]
    async fn load_template_cross_project_denied() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // 跨项目模板必须被项目过滤拦下(镜像 ent privacy 行为)。
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.other_template_id),
                "apiKeyID": guid(env.fx.target_key_id),
            }}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    #[tokio::test]
    async fn load_template_missing_read_scope_denied() -> TestResult {
        let (schema, env) = setup(&[slug::WRITE_API_KEYS]); // 缺 read

        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.template_id),
                "apiKeyID": guid(env.fx.target_key_id),
            }}),
        )
        .await;
        assert!(first_error(resp)?.contains("read_api_keys"));
        Ok(())
    }

    #[tokio::test]
    async fn load_template_by_names() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateName": env.fx.template_name,
                "apiKeyName": env.fx.target_key_name,
            }}),
        )
        .await;
        let data = data_of(resp)?;

        // Same append-only semantics as the by-id path.
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/activeProfile")?,
            &json!("Default")
        );
        let profiles = pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles")?
            .as_array()
            .ok_or("profiles must be a list")?;
        assert_eq!(profiles.len(), 2);
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles/1/name")?,
            "Production"
        );
        Ok(())
    }

    #[tokio::test]
    async fn load_template_mixed_identifiers_and_dedup_suffix() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // id for the template, name for the key.
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.template_id),
                "apiKeyName": env.fx.target_key_name,
            }}),
        )
        .await;
        let data = data_of(resp)?;
        let profiles = pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles")?
            .as_array()
            .ok_or("profiles must be a list")?;
        assert_eq!(profiles.len(), 2);

        // name for the template, id for the key — second load appends another
        // copy with a deduplicated profile name "Production (1)".
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateName": env.fx.template_name,
                "apiKeyID": guid(env.fx.target_key_id),
            }}),
        )
        .await;
        let data = data_of(resp)?;
        let profiles = pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles")?
            .as_array()
            .ok_or("profiles must be a list")?;
        assert_eq!(profiles.len(), 3);
        assert_eq!(
            pointer(&data, "/loadApiKeyProfileTemplate/profiles/profiles/2/name")?,
            "Production (1)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn load_template_requires_exactly_one_identifier_per_target() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // Template identifier missing.
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {"apiKeyID": guid(env.fx.target_key_id)}}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of template id or name"));

        // Template identified twice.
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.template_id),
                "templateName": env.fx.template_name,
                "apiKeyID": guid(env.fx.target_key_id),
            }}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of template id or name"));

        // API key identifier missing.
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {"templateID": template_guid(env.fx.template_id)}}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));

        // API key identified twice.
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateID": template_guid(env.fx.template_id),
                "apiKeyID": guid(env.fx.target_key_id),
                "apiKeyName": env.fx.target_key_name,
            }}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));
        Ok(())
    }

    #[tokio::test]
    async fn load_template_by_name_cross_project_denied() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS, slug::WRITE_API_KEYS]);

        // 外项目的模板 name 同样被项目过滤挡下。
        let resp = exec(
            &schema,
            &env,
            LOAD_MUTATION,
            json!({"input": {
                "templateName": env.fx.other_template_name,
                "apiKeyName": env.fx.target_key_name,
            }}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apiKeyQuotaUsages — mirrors TestOpenAPIResolver_APIKeyQuotaUsages_*.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn quota_usages_by_id_zero_usage_all_time_window() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);
        env.mem.set_key_quota_profile(env.fx.target_key_id)?;

        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.target_key_id)}),
        )
        .await;
        let data = data_of(resp)?;

        let usages = pointer(&data, "/apiKeyQuotaUsages")?
            .as_array()
            .ok_or("usages must be a list")?;
        assert_eq!(usages.len(), 1);
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/profileName")?,
            "Default"
        );
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/quota/requests")?,
            &json!(100)
        );
        // No usage rows → zero usage; Decimal zero renders as the number 0.
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/requestCount")?,
            &json!(0)
        );
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/totalTokens")?,
            &json!(0)
        );
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/totalCost")?,
            &json!(0)
        );
        // all_time window: open start, end = now.
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/window/start")?,
            &Value::Null
        );
        assert!(pointer(&data, "/apiKeyQuotaUsages/0/window/end")?.is_string());
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_by_key() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);
        env.mem.set_key_quota_profile(env.fx.target_key_id)?;

        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"key": env.fx.target_key}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/profileName")?,
            "Default"
        );
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_by_name() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);
        env.mem.set_key_quota_profile(env.fx.target_key_id)?;

        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"name": env.fx.target_key_name}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/profileName")?,
            "Default"
        );
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_seeded_usage_renders_decimal_as_raw_number() -> TestResult {
        // Mirrors the Go e2e numbers: requestCount=2, totalTokens=300,
        // totalCost renders as the raw number 2 (not the string "2").
        let (schema, env) = setup(&[slug::READ_API_KEYS]);
        env.mem.set_key_quota_profile(env.fx.target_key_id)?;
        env.mem.seed_usage(
            env.fx.target_key_id,
            SeededUsage {
                request_count: 2,
                total_tokens: 300,
                total_cost: rust_decimal::Decimal::from(2),
            },
        )?;

        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.target_key_id)}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/requestCount")?,
            &json!(2)
        );
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/totalTokens")?,
            &json!(300)
        );
        assert_eq!(
            pointer(&data, "/apiKeyQuotaUsages/0/usage/totalCost")?,
            &json!(2)
        );
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_no_quota_returns_empty_list() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        // targetKey fixture has a "Default" profile without quota.
        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.target_key_id)}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(pointer(&data, "/apiKeyQuotaUsages")?, &json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_cross_project_denied_by_id_and_key() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        // Foreign-project key id is filtered out by the project boundary.
        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.other_key_id)}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));

        // Same boundary for plaintext-key lookup (uniform NotFound).
        let resp = exec(&schema, &env, QUOTA_QUERY, json!({"key": env.fx.other_key})).await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_missing_read_scope_denied() -> TestResult {
        let (schema, env) = setup(&[slug::WRITE_API_KEYS]); // 缺 read

        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.target_key_id)}),
        )
        .await;
        assert!(first_error(resp)?.contains("read_api_keys"));
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_requires_exactly_one_arg() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        // Neither provided.
        let resp = exec(&schema, &env, QUOTA_QUERY, json!({})).await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));

        // Both provided.
        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": guid(env.fx.target_key_id), "key": env.fx.target_key}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));
        Ok(())
    }

    #[tokio::test]
    async fn quota_usages_invalid_guid_type() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        // A GUID of the wrong type must be rejected before any lookup.
        let resp = exec(
            &schema,
            &env,
            QUOTA_QUERY,
            json!({"id": format!("gid://conduit/Channel/{}", env.fx.target_key_id)}),
        )
        .await;
        assert!(first_error(resp)?.contains("expected a APIKey GUID, got Channel"));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apiKey — mirrors TestOpenAPIResolver_APIKey_*.
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn api_key_by_id() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"id": guid(env.fx.target_key_id)}),
        )
        .await;
        let data = data_of(resp)?;

        assert_eq!(
            pointer(&data, "/apiKey/id")?,
            &json!(guid(env.fx.target_key_id))
        );
        assert_eq!(pointer(&data, "/apiKey/key")?, &json!(env.fx.target_key));
        assert_eq!(
            pointer(&data, "/apiKey/name")?,
            &json!(env.fx.target_key_name)
        );
        assert!(pointer(&data, "/apiKey/profiles")?.is_object());
        Ok(())
    }

    #[tokio::test]
    async fn api_key_by_key() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"key": env.fx.target_key}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(
            pointer(&data, "/apiKey/id")?,
            &json!(guid(env.fx.target_key_id))
        );
        assert_eq!(
            pointer(&data, "/apiKey/name")?,
            &json!(env.fx.target_key_name)
        );
        Ok(())
    }

    #[tokio::test]
    async fn api_key_by_name() -> TestResult {
        // The headline use case: resolve id/key/profiles from the name alone.
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"name": env.fx.target_key_name}),
        )
        .await;
        let data = data_of(resp)?;
        assert_eq!(
            pointer(&data, "/apiKey/id")?,
            &json!(guid(env.fx.target_key_id))
        );
        assert_eq!(pointer(&data, "/apiKey/key")?, &json!(env.fx.target_key));
        assert_eq!(
            pointer(&data, "/apiKey/name")?,
            &json!(env.fx.target_key_name)
        );
        assert!(pointer(&data, "/apiKey/profiles")?.is_object());
        Ok(())
    }

    #[tokio::test]
    async fn api_key_requires_exactly_one_arg() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        // None provided.
        let resp = exec(&schema, &env, API_KEY_QUERY, json!({})).await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));

        // Two provided (id + key).
        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"id": guid(env.fx.target_key_id), "key": env.fx.target_key}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));

        // Two provided (key + name).
        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"key": env.fx.target_key, "name": env.fx.target_key_name}),
        )
        .await;
        assert!(first_error(resp)?.contains("exactly one of api key id, key, or name"));
        Ok(())
    }

    #[tokio::test]
    async fn api_key_cross_project_denied() -> TestResult {
        // A02: foreign-project keys stay invisible whichever identifier is
        // used — uniform NotFound, no existence leak.
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"id": guid(env.fx.other_key_id)}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"name": env.fx.other_key_name}),
        )
        .await;
        assert!(first_error(resp)?.contains("not found"));
        Ok(())
    }

    #[tokio::test]
    async fn api_key_missing_read_scope_denied() -> TestResult {
        let (schema, env) = setup(&[slug::WRITE_API_KEYS]); // 缺 read

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"name": env.fx.target_key_name}),
        )
        .await;
        assert!(first_error(resp)?.contains("read_api_keys"));
        Ok(())
    }

    #[tokio::test]
    async fn api_key_invalid_guid_type() -> TestResult {
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(
            &schema,
            &env,
            API_KEY_QUERY,
            json!({"id": format!("gid://conduit/Channel/{}", env.fx.target_key_id)}),
        )
        .await;
        assert!(first_error(resp)?.contains("expected a APIKey GUID, got Channel"));
        Ok(())
    }

    #[tokio::test]
    async fn api_key_malformed_guid_rejected() -> TestResult {
        // Mirrors objects.GUID.UnmarshalGQL's prefix guard.
        let (schema, env) = setup(&[slug::READ_API_KEYS]);

        let resp = exec(&schema, &env, API_KEY_QUERY, json!({"id": "not-a-guid"})).await;
        assert!(first_error(resp)?.contains("guid must start with gid://conduit/"));
        Ok(())
    }
}

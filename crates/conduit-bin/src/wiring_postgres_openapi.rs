//! PostgreSQL construction for the internal OpenAPI GraphQL service bundle.
//!
//! Business behavior lives in [`crate::wiring_openapi`]. This module only
//! selects PostgreSQL repositories, so the public and admin schemas cannot drift on
//! project authorization, profile serialization, or quota-window semantics.

use std::sync::Arc;

use conduit_db::repo::ApiKeyRepo;
use conduit_db::repo::profile_template_repo::ProfileTemplateRepo;
use conduit_db::{PgApiKeyRepo, PgProfileTemplateRepo, PgUsageRepo, UsageRepo};
use conduit_openapi_graphql::OpenApiServices;
use sqlx::PgPool;

/// Build the `/openapi/v1/graphql` service bundle over PostgreSQL.
pub fn build_postgres_openapi_services(pool: PgPool, key_prefix: String) -> OpenApiServices {
    let api_keys: Arc<dyn ApiKeyRepo> = Arc::new(PgApiKeyRepo::new(pool.clone()));
    let templates: Arc<dyn ProfileTemplateRepo> =
        Arc::new(PgProfileTemplateRepo::new(pool.clone()));
    let usage: Arc<dyn UsageRepo> = Arc::new(PgUsageRepo::new(pool));

    crate::wiring_openapi::build_openapi_services_from_repos(api_keys, templates, usage, key_prefix)
}

#[cfg(test)]
mod tests {
    use async_graphql::{Request, Variables};
    use conduit_auth::Principal;
    use conduit_auth::scopes::slug;
    use conduit_db::repo::profile_template_repo::{
        CreateProfileTemplateInput, ProfileTemplateRepo,
    };
    use conduit_db::{PgProfileTemplateRepo, PolicyContext, Principal as DbPrincipal};
    use serde_json::{Value, json};
    use sqlx::types::Json;
    use uuid::Uuid;

    use super::*;

    type TestError = Box<dyn std::error::Error>;

    const CREATE_KEY: &str = r#"
        mutation Create($name: String!) {
          createLLMAPIKey(name: $name) { id key name scopes }
        }
    "#;

    const READ_KEY: &str = r#"
        query Read($id: ID, $key: String, $name: String) {
          apiKey(id: $id, key: $key, name: $name) {
            id key name scopes
            profiles {
              activeProfile
              profiles {
                name
                modelMappings { from to }
                channelIDs
                modelIDs
                validFrom
                validUntil
                quota {
                  requests totalTokens cost
                  period { type }
                }
                loadBalanceStrategy
              }
            }
          }
        }
    "#;

    const UPDATE_PROFILES: &str = r#"
        mutation Update($id: ID, $name: String, $input: UpdateAPIKeyProfilesInput!) {
          updateAPIKeyProfiles(id: $id, name: $name, input: $input) {
            id name
            profiles {
              activeProfile
              profiles {
                name
                modelMappings { from to }
                channelIDs
                modelIDs
                validFrom
                validUntil
                quota {
                  requests totalTokens cost
                  period { type }
                }
                loadBalanceStrategy
              }
            }
          }
        }
    "#;

    const QUOTA_USAGE: &str = r#"
        query Usage($id: ID, $key: String, $name: String) {
          apiKeyQuotaUsages(apiKeyId: $id, key: $key, name: $name) {
            profileName
            quota { requests totalTokens cost period { type } }
            window { start end }
            usage { requestCount totalTokens totalCost }
          }
        }
    "#;

    const LOAD_TEMPLATE: &str = r#"
        mutation Load($input: LoadApiKeyProfileTemplateInput!) {
          loadApiKeyProfileTemplate(input: $input) {
            id name
            profiles { activeProfile profiles { name channelIDs modelIDs } }
          }
        }
    "#;

    fn caller(id: &str, project_id: i64) -> Principal {
        Principal::api_key_service_account(id, project_id.to_string())
            .with_scope(slug::READ_API_KEYS)
            .with_scope(slug::WRITE_API_KEYS)
    }

    fn repo_context() -> conduit_db::RequestContext {
        conduit_db::RequestContext::new(PolicyContext::new(DbPrincipal::test()))
    }

    async fn execute(
        schema: &conduit_openapi_graphql::OpenApiSchema,
        principal: &Principal,
        document: &str,
        variables: Value,
    ) -> async_graphql::Response {
        schema
            .execute(
                Request::new(document)
                    .variables(Variables::from_json(variables))
                    .data(principal.clone()),
            )
            .await
    }

    fn response_data(response: async_graphql::Response) -> Result<Value, TestError> {
        if !response.errors.is_empty() {
            return Err(format!("unexpected GraphQL errors: {:?}", response.errors).into());
        }
        Ok(response.data.into_json()?)
    }

    fn response_error(response: async_graphql::Response) -> Result<String, TestError> {
        response
            .errors
            .first()
            .map(|error| error.message.clone())
            .ok_or_else(|| "expected a GraphQL error".into())
    }

    fn pointer<'a>(value: &'a Value, path: &str) -> Result<&'a Value, TestError> {
        value
            .pointer(path)
            .ok_or_else(|| format!("missing {path} in {value}").into())
    }

    fn api_key_guid(id: i64) -> String {
        format!("gid://conduit/APIKey/{id}")
    }

    fn template_guid(id: i64) -> String {
        format!("gid://conduit/APIKeyProfileTemplate/{id}")
    }

    async fn create_key(
        schema: &conduit_openapi_graphql::OpenApiSchema,
        principal: &Principal,
        name: &str,
    ) -> Result<(i64, String, String), TestError> {
        let data =
            response_data(execute(schema, principal, CREATE_KEY, json!({"name": name})).await)?;
        let guid = pointer(&data, "/createLLMAPIKey/id")?
            .as_str()
            .ok_or("created API key GUID must be a string")?
            .to_string();
        let id = guid
            .rsplit('/')
            .next()
            .ok_or("created API key GUID lacks an id")?
            .parse::<i64>()?;
        let key = pointer(&data, "/createLLMAPIKey/key")?
            .as_str()
            .ok_or("created API key secret must be a string")?
            .to_string();
        Ok((id, guid, key))
    }

    #[tokio::test]
    async fn postgres_openapi_schema_enforces_project_scope_and_profile_quota_contract()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let project_one = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, description, status, profiles) \
             VALUES ($1, '', 'active', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("OpenAPI P1 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let project_two = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, description, status, profiles) \
             VALUES ($1, '', 'active', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("OpenAPI P2 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\", name, status, credentials, supported_models, default_test_model) \
             VALUES ('openai', $1, 'enabled', '{}'::jsonb, '[]'::jsonb, '') RETURNING id",
        )
        .bind(format!("OpenAPI Channel {suffix}"))
        .fetch_one(&pool)
        .await?;
        let model_id = format!("openapi-model-{suffix}");
        sqlx::query(
            "INSERT INTO models \
             (developer, model_id, \"type\", name, icon, \"group\", model_card, settings, status) \
             VALUES ('test', $1, 'chat', $2, '', 'test', '{}'::jsonb, '{}'::jsonb, 'enabled')",
        )
        .bind(&model_id)
        .bind(format!("OpenAPI Model {suffix}"))
        .execute(&pool)
        .await?;

        let services = build_postgres_openapi_services(pool.clone(), "conduit".to_string());
        let schema = conduit_openapi_graphql::build_openapi_schema(services);
        let principal_one = caller("openapi-owner-one", project_one);
        let principal_two = caller("openapi-owner-two", project_two);

        let (key_one_id, key_one_guid, key_one_secret) =
            create_key(&schema, &principal_one, &format!("OpenAPI Key P1 {suffix}")).await?;
        let (key_two_id, key_two_guid, _) =
            create_key(&schema, &principal_two, &format!("OpenAPI Key P2 {suffix}")).await?;

        let invalid_update = execute(
            &schema,
            &principal_one,
            UPDATE_PROFILES,
            json!({
                "id": key_one_guid,
                "input": {
                    "activeProfile": "Missing",
                    "profiles": [{"name": "Scoped", "modelIDs": [model_id]}]
                }
            }),
        )
        .await;
        let invalid_error = response_error(invalid_update)?;
        assert!(
            invalid_error.contains("active profile 'Missing' does not exist"),
            "unexpected profile validation error: {invalid_error}"
        );

        let scoped_profile = json!({
            "activeProfile": "Scoped",
            "profiles": [{
                "name": "Scoped",
                "modelMappings": [{"from": "public-model", "to": "upstream-model"}],
                "channelIDs": [channel_id],
                "modelIDs": [model_id],
                "validFrom": "2026-08-15T00:00:00Z",
                "validUntil": "2026-09-15T00:00:00Z",
                "quota": {
                    "requests": 50,
                    "totalTokens": 5000,
                    "cost": "25.5",
                    "period": {"type": "all_time"}
                },
                "loadBalanceStrategy": "round_robin"
            }]
        });
        let updated = response_data(
            execute(
                &schema,
                &principal_one,
                UPDATE_PROFILES,
                json!({"id": key_one_guid, "input": scoped_profile}),
            )
            .await,
        )?;
        assert_eq!(
            pointer(
                &updated,
                "/updateAPIKeyProfiles/profiles/profiles/0/channelIDs/0"
            )?,
            &json!(channel_id)
        );
        assert_eq!(
            pointer(
                &updated,
                "/updateAPIKeyProfiles/profiles/profiles/0/modelIDs/0"
            )?,
            &json!(model_id)
        );
        assert_eq!(
            pointer(
                &updated,
                "/updateAPIKeyProfiles/profiles/profiles/0/quota/requests"
            )?,
            &json!(50)
        );
        let valid_until = pointer(
            &updated,
            "/updateAPIKeyProfiles/profiles/profiles/0/validUntil",
        )?
        .as_str()
        .ok_or("validUntil must be an RFC 3339 string")?;
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(valid_until)?,
            chrono::DateTime::parse_from_rfc3339("2026-09-15T00:00:00Z")?
        );

        // Read-back travels through the schema and the PostgreSQL repository,
        // proving that model/channel/time/quota limits were actually persisted.
        let read_back = response_data(
            execute(
                &schema,
                &principal_one,
                READ_KEY,
                json!({"key": key_one_secret}),
            )
            .await,
        )?;
        assert_eq!(
            pointer(&read_back, "/apiKey/profiles/activeProfile")?,
            &json!("Scoped")
        );

        sqlx::query(
            "INSERT INTO usage_logs \
             (request_id, api_key_id, project_id, channel_id, model_id, \
              prompt_tokens, completion_tokens, total_tokens, total_cost) \
             VALUES ($1, $2, $3, $4, $5, 80, 40, 120, 2.5)",
        )
        .bind(9_000_000_i64 + key_one_id)
        .bind(key_one_id)
        .bind(project_one)
        .bind(channel_id)
        .bind(&model_id)
        .execute(&pool)
        .await?;
        let quota = response_data(
            execute(
                &schema,
                &principal_one,
                QUOTA_USAGE,
                json!({"id": api_key_guid(key_one_id)}),
            )
            .await,
        )?;
        assert_eq!(
            pointer(&quota, "/apiKeyQuotaUsages/0/usage/requestCount")?,
            &json!(1)
        );
        assert_eq!(
            pointer(&quota, "/apiKeyQuotaUsages/0/usage/totalTokens")?,
            &json!(120)
        );
        assert_eq!(
            pointer(&quota, "/apiKeyQuotaUsages/0/usage/totalCost")?,
            &json!(2.5)
        );

        // A project-two principal cannot read project one's key by either
        // identifier form and cannot update it. Foreign and missing are both
        // the same NotFound surface, preventing existence leaks.
        for variables in [
            json!({"id": api_key_guid(key_one_id)}),
            json!({"key": key_one_secret}),
        ] {
            let error =
                response_error(execute(&schema, &principal_two, READ_KEY, variables).await)?;
            assert_eq!(error, "api_key not found");
        }
        let foreign_update = response_error(
            execute(
                &schema,
                &principal_two,
                UPDATE_PROFILES,
                json!({
                    "id": api_key_guid(key_one_id),
                    "input": {"activeProfile": "Hijacked", "profiles": []}
                }),
            )
            .await,
        )?;
        assert_eq!(foreign_update, "api_key not found");
        let active_profile: String =
            sqlx::query_scalar("SELECT profiles->>'activeProfile' FROM api_keys WHERE id = $1")
                .bind(key_one_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(active_profile, "Scoped");

        let template_repo = PgProfileTemplateRepo::new(pool.clone());
        let template_one = template_repo
            .create_profile_template(
                &repo_context(),
                CreateProfileTemplateInput {
                    project_id: project_one.to_string(),
                    name: format!("Template P1 {suffix}"),
                    description: None,
                    profile: Some(json!({
                        "name": "Template Scope",
                        "modelMappings": [],
                        "channelIDs": [channel_id],
                        "modelIDs": [model_id]
                    })),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await?;
        let template_two = template_repo
            .create_profile_template(
                &repo_context(),
                CreateProfileTemplateInput {
                    project_id: project_two.to_string(),
                    name: format!("Template P2 {suffix}"),
                    description: None,
                    profile: Some(json!({"name": "Foreign", "modelMappings": []})),
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await?;

        let loaded = response_data(
            execute(
                &schema,
                &principal_one,
                LOAD_TEMPLATE,
                json!({"input": {
                    "templateID": template_guid(template_one.id.parse()?),
                    "apiKeyID": api_key_guid(key_one_id)
                }}),
            )
            .await,
        )?;
        assert_eq!(
            pointer(
                &loaded,
                "/loadApiKeyProfileTemplate/profiles/profiles/1/name"
            )?,
            &json!("Template Scope")
        );

        let foreign_template = response_error(
            execute(
                &schema,
                &principal_one,
                LOAD_TEMPLATE,
                json!({"input": {
                    "templateID": template_guid(template_two.id.parse()?),
                    "apiKeyID": api_key_guid(key_one_id)
                }}),
            )
            .await,
        )?;
        assert_eq!(foreign_template, "api_key_profile_template not found");
        let foreign_key = response_error(
            execute(
                &schema,
                &principal_one,
                LOAD_TEMPLATE,
                json!({"input": {
                    "templateID": template_guid(template_one.id.parse()?),
                    "apiKeyID": key_two_guid
                }}),
            )
            .await,
        )?;
        assert_eq!(foreign_key, "api_key not found");
        let project_two_profiles: Json<Value> =
            sqlx::query_scalar("SELECT profiles FROM api_keys WHERE id = $1")
                .bind(key_two_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(project_two_profiles.0, json!({}));

        // The frozen OpenAPI contract intentionally exposes no delete-key
        // mutation, so a project cannot use this surface to delete any key —
        // foreign or otherwise.
        let delete_error = response_error(
            execute(
                &schema,
                &principal_two,
                "mutation { deleteAPIKey(id: \"gid://conduit/APIKey/1\") { id } }",
                json!({}),
            )
            .await,
        )?;
        assert!(delete_error.contains("deleteAPIKey"), "{delete_error}");

        pool.close().await;
        Ok(())
    }
}

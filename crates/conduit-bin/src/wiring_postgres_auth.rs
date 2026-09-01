//! PostgreSQL authentication adapters used by host wiring.

use async_trait::async_trait;
use conduit_auth::Scope;
use conduit_auth::encode_password_bcrypt_hex;
use conduit_core::objects::apikey::{APIKeyProfile, APIKeyProfiles};
use conduit_db::PgApiKeyRepo;
use conduit_db::pg_quota_admission::{
    PgQuotaAdmission, PgQuotaAdmissionOutcome, admit_postgres_request,
};
use conduit_db::repo::{ApiKeyRepo, RepoError, RequestContext};
use conduit_http::auth_handlers::{
    AuthenticatedUser, SignUpRequest, SigninService, SignupError, SignupService,
};
use conduit_http::middleware::api_key_auth::{
    ApiKeyValidationError, ApiKeyValidationService, ValidatedApiKeyMetadata,
};
use conduit_services::apikey_service::validate_all_profiles;
use conduit_services::{
    AuthApiKey, AuthApiKeyRepo, AuthApiKeyStatus, AuthApiKeyType, AuthProjectStatus,
    AuthServiceError, AuthUser, AuthUserRepo, AuthUserStatus,
};
use conduit_services::{AuthService, QuotaPeriod};
use rust_decimal::prelude::FromPrimitive;
use sqlx::PgPool;
use std::collections::BTreeSet;
use std::sync::Arc;

pub(crate) struct PgApiKeyValidationService {
    auth: Arc<AuthService>,
    api_keys: Arc<PgApiKeyRepo>,
    pool: PgPool,
}

impl PgApiKeyValidationService {
    pub(crate) fn new(auth: Arc<AuthService>, api_keys: Arc<PgApiKeyRepo>, pool: PgPool) -> Self {
        Self {
            auth,
            api_keys,
            pool,
        }
    }

    async fn check_quota_and_admit(
        &self,
        key_id: i64,
        project_id: i64,
        profile: &APIKeyProfile,
    ) -> Result<(), ApiKeyValidationError> {
        let Some(quota) = profile.quota.as_ref() else {
            return Ok(());
        };
        let now = chrono::Utc::now();
        let window = QuotaPeriod::from_core(&quota.period)
            .and_then(|period| period.window(now))
            .map_err(|_| ApiKeyValidationError::Internal)?;
        let (tokens, cost): (i64, f64) = sqlx::query_as(
            "SELECT COALESCE(SUM(total_tokens),0)::BIGINT,COALESCE(SUM(total_cost),0)::DOUBLE PRECISION \
             FROM usage_logs WHERE project_id=$1 AND api_key_id=$2 \
             AND ($3::timestamptz IS NULL OR created_at >= $3) \
             AND ($4::timestamptz IS NULL OR created_at < $4 OR ($5 AND created_at=$4))",
        )
        .bind(project_id).bind(key_id).bind(window.start).bind(window.end).bind(window.end_inclusive)
        .fetch_one(&self.pool).await.map_err(|_| ApiKeyValidationError::Internal)?;
        if quota
            .total_tokens
            .is_some_and(|limit| limit >= 0 && tokens >= limit)
        {
            return Err(ApiKeyValidationError::QuotaExceeded);
        }
        let used_cost =
            rust_decimal::Decimal::from_f64(cost).ok_or(ApiKeyValidationError::Internal)?;
        if quota
            .cost
            .is_some_and(|limit| limit >= rust_decimal::Decimal::ZERO && used_cost >= limit)
        {
            return Err(ApiKeyValidationError::QuotaExceeded);
        }
        let outcome = admit_postgres_request(
            &self.pool,
            &PgQuotaAdmission {
                api_key_id: key_id,
                project_id,
                profile_name: &profile.name,
                start: window.start,
                end: window.end,
                end_inclusive: window.end_inclusive,
                request_limit: quota.requests.unwrap_or(i64::MAX),
                admitted_at: now,
            },
        )
        .await
        .map_err(|_| ApiKeyValidationError::Internal)?;
        if outcome == PgQuotaAdmissionOutcome::Exceeded {
            return Err(ApiKeyValidationError::QuotaExceeded);
        }
        Ok(())
    }
}

fn parse_validated_profiles(
    api_key_id: &str,
    value: &serde_json::Value,
) -> Result<APIKeyProfiles, ApiKeyValidationError> {
    let profiles: APIKeyProfiles = serde_json::from_value(value.clone()).map_err(|_| {
        tracing::warn!(api_key_id, "rejecting API key with malformed profile state");
        ApiKeyValidationError::Invalid
    })?;
    if let Err(error) = validate_all_profiles(&profiles) {
        tracing::warn!(
            api_key_id,
            reason = %error,
            "rejecting API key with invalid profile state"
        );
        return Err(ApiKeyValidationError::Invalid);
    }
    Ok(profiles)
}

#[async_trait]
impl ApiKeyValidationService for PgApiKeyValidationService {
    async fn validate(
        &self,
        plaintext_key: &str,
    ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError> {
        let ctx = RequestContext::new(conduit_db::PolicyContext::new(
            conduit_db::Principal::system(),
        ));
        let authenticated = self
            .auth
            .authenticate_api_key(&ctx, plaintext_key)
            .await
            .map_err(|e| match e {
                AuthServiceError::InvalidCredentials
                | AuthServiceError::ApiKeyInactive(_)
                | AuthServiceError::ProjectInactive(_)
                | AuthServiceError::NoAuthApiKeyRejected => ApiKeyValidationError::Invalid,
                _ => ApiKeyValidationError::Internal,
            })?;
        let row = self
            .api_keys
            .find_api_key_by_id(&ctx, &authenticated.id)
            .await
            .map_err(|_| ApiKeyValidationError::Internal)?
            .ok_or(ApiKeyValidationError::Invalid)?;
        let profiles = parse_validated_profiles(&row.id, &row.profiles)?;
        let active = if profiles.active_profile.is_empty() {
            None
        } else {
            // The domain validator above guarantees that every non-empty
            // selection resolves. Keep the explicit fail-closed branch here
            // so future validator changes cannot reintroduce a bypass.
            Some(
                profiles
                    .profiles
                    .iter()
                    .find(|profile| profile.name == profiles.active_profile)
                    .ok_or(ApiKeyValidationError::Invalid)?,
            )
        };
        let now = chrono::Utc::now();
        if active.is_some_and(|p| {
            p.valid_from.is_some_and(|v| now < v) || p.valid_until.is_some_and(|v| now >= v)
        }) {
            return Err(ApiKeyValidationError::Invalid);
        }
        let project_id = authenticated
            .project_id
            .parse::<i64>()
            .map_err(|_| ApiKeyValidationError::Internal)?;
        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &self.pool, project_id,
        )
        .await
        .map_err(|_| ApiKeyValidationError::Internal)?;
        let key_models = active
            .filter(|p| !p.model_ids.is_empty())
            .map(|p| p.model_ids.iter().cloned().collect::<BTreeSet<_>>());
        let key_channels = active
            .map(|p| p.channel_ids.iter().copied().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let mut channels_by_model = std::collections::BTreeMap::new();
        let mut upstreams_by_model = std::collections::BTreeMap::new();
        for (model, channels) in &access.routes_by_model {
            if key_models
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(model))
            {
                continue;
            }
            let effective = channels
                .iter()
                .copied()
                .filter(|id| key_channels.is_empty() || key_channels.contains(id))
                .collect::<Vec<_>>();
            if effective.is_empty() {
                continue;
            }
            channels_by_model.insert(model.clone(), effective.clone());
            let upstreams = access
                .upstream_models_for_model(model)
                .into_iter()
                .filter(|(id, _)| effective.contains(id))
                .collect::<std::collections::BTreeMap<_, _>>();
            if !upstreams.is_empty() {
                upstreams_by_model.insert(model.clone(), upstreams);
            }
        }
        if channels_by_model.is_empty() {
            return Err(ApiKeyValidationError::Invalid);
        }
        if let Some(profile) = active {
            self.check_quota_and_admit(
                row.id
                    .parse()
                    .map_err(|_| ApiKeyValidationError::Internal)?,
                project_id,
                profile,
            )
            .await?;
        }
        let allowed_models = channels_by_model
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        let project_channel_ids = access
            .routes_by_model
            .values()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let model_mapping = active
            .map(|p| {
                p.model_mappings
                    .iter()
                    .map(|m| (m.from.clone(), m.to.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .and_then(|m| serde_json::to_string(&m).ok())
            .unwrap_or_default();
        Ok(ValidatedApiKeyMetadata {
            api_key_id: row
                .id
                .parse()
                .map_err(|_| ApiKeyValidationError::Internal)?,
            api_key_name: authenticated.name,
            key_type: row.key_type,
            scopes: row.scopes,
            allowed_models,
            project_id,
            model_mapping,
            key_channel_ids: active.map(|p| p.channel_ids.clone()).unwrap_or_default(),
            key_channel_tags: active.map(|p| p.channel_tags.clone()).unwrap_or_default(),
            key_channel_tags_match_mode: active
                .and_then(|p| p.channel_tags_match_mode.clone())
                .unwrap_or_default(),
            project_channel_ids,
            project_channels_by_model: channels_by_model,
            project_upstream_models_by_model: upstreams_by_model,
            project_channel_tags: Vec::new(),
            project_channel_tags_match_mode: String::new(),
            load_balance_strategy: active
                .and_then(|p| p.load_balance_strategy.clone())
                .unwrap_or_default(),
            quota_rpm: active.and_then(profile_rpm),
            max_concurrent_requests: active.and_then(|profile| profile.max_concurrent_requests),
        })
    }
}

fn profile_rpm(profile: &APIKeyProfile) -> Option<i64> {
    use conduit_core::objects::apikey::{
        api_key_quota_past_duration_unit, api_key_quota_period_type,
    };
    let quota = profile.quota.as_ref()?;
    let duration = quota.period.past_duration.as_ref()?;
    (quota.period.r#type == api_key_quota_period_type::PAST_DURATION
        && duration.unit == api_key_quota_past_duration_unit::MINUTE
        && duration.value == 1)
        .then_some(quota.requests)
        .flatten()
}

fn database(context: &str, error: sqlx::Error) -> AuthServiceError {
    AuthServiceError::Repo(RepoError::Database(format!(
        "postgres auth {context} failed: {error}"
    )))
}

#[derive(Debug, Clone)]
pub(crate) struct PgAuthUserRepo {
    pool: PgPool,
}

impl PgAuthUserRepo {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn load(
        &self,
        email: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Option<AuthUser>, AuthServiceError> {
        let user_id = user_id
            .map(|value| value.parse::<i64>())
            .transpose()
            .map_err(|_| AuthServiceError::Repo(RepoError::NotFound("user id not an integer")))?;
        let row = sqlx::query_as::<_, (i64, String, String, String, Option<String>, Option<String>, bool, sqlx::types::Json<Vec<String>>)>(
            "SELECT id,email,status,password,first_name,last_name,is_owner,COALESCE(scopes,'[]'::jsonb) \
             FROM users WHERE deleted_at=0 AND (($1::text IS NOT NULL AND email=$1) OR ($2::bigint IS NOT NULL AND id=$2))")
            .bind(email).bind(user_id).fetch_optional(&self.pool).await.map_err(|e|database("user lookup",e))?;
        let Some((
            id,
            email,
            status,
            password,
            first,
            last,
            is_owner,
            sqlx::types::Json(direct_scopes),
        )) = row
        else {
            return Ok(None);
        };

        let system_roles = sqlx::query_as::<_, (sqlx::types::Json<Vec<String>>,)>(
            "SELECT COALESCE(r.scopes,'[]'::jsonb) FROM roles r JOIN user_roles ur ON ur.role_id=r.id \
             WHERE ur.user_id=$1 AND r.deleted_at=0 AND (r.project_id IS NULL OR r.project_id=0)")
            .bind(id).fetch_all(&self.pool).await.map_err(|e|database("system roles",e))?;
        let memberships = sqlx::query_as::<_, (i64, sqlx::types::Json<Vec<String>>, bool)>(
            "SELECT project_id,COALESCE(scopes,'[]'::jsonb),is_owner FROM user_projects WHERE user_id=$1",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| database("project memberships", e))?;
        let project_roles = sqlx::query_as::<_, (i64, sqlx::types::Json<Vec<String>>)>(
            "SELECT r.project_id,COALESCE(r.scopes,'[]'::jsonb) FROM roles r JOIN user_roles ur ON ur.role_id=r.id \
             JOIN user_projects up ON up.user_id=ur.user_id AND up.project_id=r.project_id \
             WHERE ur.user_id=$1 AND r.deleted_at=0 AND r.project_id IS NOT NULL AND r.project_id<>0")
            .bind(id).fetch_all(&self.pool).await.map_err(|e|database("project roles",e))?;

        let mut scopes: BTreeSet<String> = direct_scopes.into_iter().collect();
        for (sqlx::types::Json(role_scopes),) in system_roles {
            scopes.extend(
                role_scopes
                    .into_iter()
                    .map(|scope| format!("system:role:{scope}")),
            );
        }
        let mut project_ids = BTreeSet::new();
        for (project_id, sqlx::types::Json(values), is_owner) in memberships {
            let project_id = project_id.to_string();
            project_ids.insert(project_id.clone());
            scopes.extend(
                values
                    .into_iter()
                    .map(|scope| Scope::project_membership(&project_id, scope).to_string()),
            );
            if is_owner {
                scopes.insert(Scope::project_membership(&project_id, "*").to_string());
            }
        }
        for (project_id, sqlx::types::Json(values)) in project_roles {
            let project_id = project_id.to_string();
            scopes.extend(
                values
                    .into_iter()
                    .map(|scope| Scope::project_role(&project_id, scope).to_string()),
            );
        }
        Ok(Some(AuthUser {
            id: id.to_string(),
            email,
            display_name: format!(
                "{} {}",
                first.as_deref().unwrap_or(""),
                last.as_deref().unwrap_or("")
            )
            .trim()
            .to_string(),
            password_bcrypt_hex: Some(password),
            status: if status == "activated" {
                AuthUserStatus::Active
            } else {
                AuthUserStatus::Disabled
            },
            is_owner,
            scope_slugs: scopes.into_iter().collect(),
            project_ids: project_ids.into_iter().collect(),
            oidc_only: false,
        }))
    }
}

#[async_trait]
impl AuthUserRepo for PgAuthUserRepo {
    async fn find_user_by_email(
        &self,
        _: &RequestContext,
        email: &str,
    ) -> Result<Option<AuthUser>, AuthServiceError> {
        self.load(Some(email), None).await
    }
    async fn find_user_by_id(
        &self,
        _: &RequestContext,
        user_id: &str,
    ) -> Result<Option<AuthUser>, AuthServiceError> {
        self.load(None, Some(user_id)).await
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PgAuthApiKeyRepo {
    pool: PgPool,
}
impl PgAuthApiKeyRepo {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl AuthApiKeyRepo for PgAuthApiKeyRepo {
    async fn find_api_key_by_plaintext(
        &self,
        _: &RequestContext,
        key: &str,
    ) -> Result<Option<AuthApiKey>, AuthServiceError> {
        let row=sqlx::query_as::<_,(i64,String,i64,String,String,sqlx::types::Json<Vec<String>>,Option<String>)>(
            "SELECT k.id,k.name,k.project_id,k.status,k.\"type\",COALESCE(k.scopes,'[]'::jsonb),p.status \
             FROM api_keys k LEFT JOIN projects p ON p.id=k.project_id WHERE k.key=$1 AND k.deleted_at=0")
            .bind(key).fetch_optional(&self.pool).await.map_err(|e|database("api key lookup",e))?;
        Ok(row.map(
            |(
                id,
                name,
                project_id,
                status,
                key_type,
                sqlx::types::Json(scopes),
                project_status,
            )| AuthApiKey {
                id: id.to_string(),
                project_id: project_id.to_string(),
                name,
                status: match status.as_str() {
                    "enabled" => AuthApiKeyStatus::Active,
                    "archived" => AuthApiKeyStatus::Archived,
                    _ => AuthApiKeyStatus::Disabled,
                },
                project_status: match project_status.as_deref() {
                    Some("active") => AuthProjectStatus::Active,
                    Some("archived") => AuthProjectStatus::Archived,
                    _ => AuthProjectStatus::Disabled,
                },
                key_type: match key_type.as_str() {
                    "service_account" => AuthApiKeyType::ServiceAccount,
                    "noauth" => AuthApiKeyType::NoAuth,
                    _ => AuthApiKeyType::User,
                },
                scope_slugs: scopes,
            },
        ))
    }

    async fn ensure_no_auth_api_key(
        &self,
        ctx: &RequestContext,
    ) -> Result<AuthApiKey, AuthServiceError> {
        const KEY: &str = "CONDUIT_API_KEY_NO_AUTH";
        const NAME: &str = "No Auth System Key";
        if let Some(key) = self.find_api_key_by_plaintext(ctx, KEY).await? {
            return Ok(key);
        }
        let project_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM projects WHERE deleted_at=0 ORDER BY id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database("default project", e))?
        .ok_or_else(|| AuthServiceError::NoAuthApiKeyMissing("no project found".into()))?;
        let user_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM users WHERE is_owner=TRUE AND deleted_at=0 ORDER BY id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database("owner user", e))?
        .ok_or_else(|| AuthServiceError::NoAuthApiKeyMissing("no owner user found".into()))?;
        sqlx::query("INSERT INTO api_keys(user_id,project_id,key,name,\"type\",status,scopes) VALUES($1,$2,$3,$4,'noauth','enabled',$5) ON CONFLICT(key) DO NOTHING")
            .bind(user_id).bind(project_id).bind(KEY).bind(NAME).bind(sqlx::types::Json(vec!["write_requests","read_channels"])).execute(&self.pool).await.map_err(|e|database("create noauth key",e))?;
        self.find_api_key_by_plaintext(ctx, KEY)
            .await?
            .ok_or_else(|| AuthServiceError::NoAuthApiKeyMissing("created key disappeared".into()))
    }
}

pub(crate) struct PgSignupService {
    pool: PgPool,
    bcrypt_cost: u32,
    signin: Arc<dyn SigninService>,
}

impl PgSignupService {
    pub(crate) fn new(pool: PgPool, bcrypt_cost: u32, signin: Arc<dyn SigninService>) -> Self {
        Self {
            pool,
            bcrypt_cost,
            signin,
        }
    }
}

#[async_trait]
impl SignupService for PgSignupService {
    async fn register_user(
        &self,
        request: SignUpRequest,
    ) -> Result<AuthenticatedUser, SignupError> {
        let email = request.email.trim().to_lowercase();
        let password = encode_password_bcrypt_hex(&request.password, self.bcrypt_cost)
            .map_err(|e| SignupError::Internal(e.to_string()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| SignupError::Internal(e.to_string()))?;
        let initialized = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key='system_initialized' AND deleted_at=0",
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| SignupError::Internal(e.to_string()))?;
        if !initialized
            .as_deref()
            .and_then(|v| serde_json::from_str::<bool>(v).ok())
            .unwrap_or(false)
        {
            return Err(SignupError::SystemNotInitialized);
        }
        let user_id=sqlx::query_scalar::<_,i64>("INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) VALUES($1,'activated','en',$2,$3,$4,FALSE,'[]'::jsonb) RETURNING id")
            .bind(&email).bind(password).bind(request.first_name.trim()).bind(request.last_name.trim()).fetch_one(&mut *tx).await.map_err(|e|if e.as_database_error().and_then(|e|e.code()).is_some_and(|v|v=="23505"){SignupError::EmailTaken}else{SignupError::Internal(e.to_string())})?;
        let project_id=sqlx::query_scalar::<_,i64>("INSERT INTO projects(name,description,status,profiles) VALUES($1,'Private workspace created during signup','active','{}'::jsonb) RETURNING id")
            .bind(format!("Personal Workspace #{user_id}")).fetch_one(&mut *tx).await.map_err(|e|SignupError::Internal(e.to_string()))?;
        sqlx::query("INSERT INTO project_commercial_profiles(project_id,account_type,billing_currency,status,created_at,updated_at) VALUES($1,'personal','STATION_CREDIT','active',now(),now())")
            .bind(project_id).execute(&mut *tx).await.map_err(|e|SignupError::Internal(e.to_string()))?;
        let owner_scopes = vec![
            "read_api_keys",
            "write_api_keys",
            "read_requests",
            "write_requests",
        ];
        let role_id=sqlx::query_scalar::<_,i64>("INSERT INTO roles(name,level,project_id,scopes) VALUES('Owner','project',$1,$2) RETURNING id")
            .bind(project_id).bind(sqlx::types::Json(owner_scopes)).fetch_one(&mut *tx).await.map_err(|e|SignupError::Internal(e.to_string()))?;
        sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES($1,$2,TRUE,'[]'::jsonb)").bind(user_id).bind(project_id).execute(&mut *tx).await.map_err(|e|SignupError::Internal(e.to_string()))?;
        sqlx::query("INSERT INTO user_roles(user_id,role_id) VALUES($1,$2)")
            .bind(user_id)
            .bind(role_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| SignupError::Internal(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| SignupError::Internal(e.to_string()))?;
        self.signin
            .authenticate_user(&email, &request.password)
            .await
            .map_err(|e| SignupError::Internal(format!("new user authentication failed: {e:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_db::policy::{PolicyContext, Principal};
    use serde_json::json;

    #[test]
    fn persisted_profiles_fail_closed_when_active_selection_is_missing() {
        let invalid = json!({
            "activeProfile": "ghost",
            "profiles": [{ "name": "production", "modelIDs": ["restricted-model"] }]
        });

        assert_eq!(
            parse_validated_profiles("test-key", &invalid),
            Err(ApiKeyValidationError::Invalid)
        );
        assert_eq!(
            parse_validated_profiles("test-key", &json!({})),
            Ok(APIKeyProfiles::default())
        );
    }

    #[tokio::test]
    async fn postgres_auth_expands_membership_and_role_scopes_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let user_id=sqlx::query_scalar::<_,i64>("INSERT INTO users(email,status,password,is_owner,scopes)VALUES('pg-auth@example.com','activated','hash',FALSE,'[\"direct\"]'::jsonb)RETURNING id").fetch_one(&pool).await?;
        let project_id=sqlx::query_scalar::<_,i64>("INSERT INTO projects(name,status,description,profiles)VALUES('PG Auth','active','','{}'::jsonb)RETURNING id").fetch_one(&pool).await?;
        let role_id=sqlx::query_scalar::<_,i64>("INSERT INTO roles(name,level,project_id,scopes)VALUES('Developer','project',$1,'[\"read_channels\"]'::jsonb)RETURNING id").bind(project_id).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner,scopes)VALUES($1,$2,FALSE,'[\"read_models\"]'::jsonb)").bind(user_id).bind(project_id).execute(&pool).await?;
        sqlx::query("INSERT INTO user_roles(user_id,role_id)VALUES($1,$2)")
            .bind(user_id)
            .bind(role_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO api_keys(user_id,project_id,key,name,\"type\",status,scopes)VALUES($1,$2,'pg-key','PG Key','user','enabled','[\"write_requests\"]'::jsonb)").bind(user_id).bind(project_id).execute(&pool).await?;
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let user = PgAuthUserRepo::new(pool.clone())
            .find_user_by_email(&ctx, "pg-auth@example.com")
            .await?
            .unwrap();
        assert!(user.project_ids.contains(&project_id.to_string()));
        assert!(user.scope_slugs.iter().any(|v| v.contains("read_models")));
        assert!(user.scope_slugs.iter().any(|v| v.contains("read_channels")));
        let key = PgAuthApiKeyRepo::new(pool)
            .find_api_key_by_plaintext(&ctx, "pg-key")
            .await?
            .unwrap();
        assert_eq!(key.project_status, AuthProjectStatus::Active);
        isolated.cleanup().await?;
        Ok(())
    }
}

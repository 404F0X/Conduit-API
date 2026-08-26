//! PostgreSQL-backed current-user query and mutation adapters.
//!
//! The admin GraphQL traits deliberately do not depend on a database driver.
//! This module owns the PostgreSQL identity bridge used by
//! `wiring.rs`; it keeps PostgreSQL-native booleans, JSONB and timestamps all
//! the way through the query boundary.

use std::collections::BTreeSet;

use async_trait::async_trait;
use conduit_admin_graphql::me::{MeError, OidcIdentityInfo, RoleInfo, UserInfo, UserProjectInfo};
use conduit_admin_graphql::me_ext::{MeMutationError, UpdateMeInput};
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::project::Project;
use conduit_admin_graphql::scalars::TimeScalar;
use conduit_admin_graphql::user::{User, UserStatus};
use conduit_db::row::ProjectRow;
use sqlx::PgPool;
use sqlx::types::Json;

/// PostgreSQL implementation of `Query.me` and `Query.myProjects`.
#[derive(Debug, Clone)]
pub(crate) struct PgMeServiceAdapter {
    pool: PgPool,
}

impl PgMeServiceAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn role_is_system(project_id: Option<i64>) -> bool {
    project_id.is_none_or(|id| id == 0)
}

#[async_trait]
impl conduit_admin_graphql::me::MeServices for PgMeServiceAdapter {
    async fn me(&self, user_id: i64) -> Result<UserInfo, MeError> {
        let user_row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                Option<String>,
                bool,
                Json<Vec<String>>,
                String,
            ),
        >(
            "SELECT email, prefer_language, first_name, last_name, avatar, \
                    is_owner, COALESCE(scopes, '[]'::jsonb), password \
             FROM users WHERE id = $1 AND deleted_at = 0",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| MeError::UserDetails(error.to_string()))?;

        let Some((
            email,
            prefer_language,
            first_name,
            last_name,
            avatar,
            is_owner,
            Json(direct_scopes),
            password,
        )) = user_row
        else {
            return Err(MeError::UserDetails("user not found".to_string()));
        };

        let role_rows = sqlx::query_as::<_, (String, Option<i64>, Json<Vec<String>>)>(
            "SELECT r.name, r.project_id, COALESCE(r.scopes, '[]'::jsonb) \
             FROM roles r JOIN user_roles ur ON ur.role_id = r.id \
             WHERE ur.user_id = $1 AND r.deleted_at = 0 \
             ORDER BY r.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| MeError::UserDetails(error.to_string()))?;

        let project_rows = sqlx::query_as::<_, (i64, bool, Json<Vec<String>>)>(
            "SELECT project_id, is_owner, COALESCE(scopes, '[]'::jsonb) \
             FROM user_projects WHERE user_id = $1 ORDER BY project_id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| MeError::UserDetails(error.to_string()))?;

        let oidc_rows = sqlx::query_as::<_, (i64, Option<String>, String, String, Option<String>)>(
            "SELECT id, idp_name, issuer, subject, email \
             FROM oidc_identities \
             WHERE user_id = $1 AND deleted_at = 0 ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| MeError::UserDetails(error.to_string()))?;

        let mut global_scopes: BTreeSet<String> = direct_scopes.into_iter().collect();
        let mut global_roles = Vec::new();
        for (name, project_id, Json(role_scopes)) in &role_rows {
            if role_is_system(*project_id) {
                global_roles.push(RoleInfo { name: name.clone() });
                global_scopes.extend(role_scopes.iter().cloned());
            }
        }

        let projects = project_rows
            .into_iter()
            .map(
                |(project_id, project_is_owner, Json(direct_project_scopes))| {
                    let matching_roles = role_rows
                        .iter()
                        .filter(|(_, role_project_id, _)| {
                            !role_is_system(*role_project_id)
                                && *role_project_id == Some(project_id)
                        })
                        .collect::<Vec<_>>();
                    let roles = matching_roles
                        .iter()
                        .map(|(name, _, _)| RoleInfo { name: name.clone() })
                        .collect();
                    let mut scopes: BTreeSet<String> = direct_project_scopes.into_iter().collect();
                    for (_, _, Json(role_scopes)) in matching_roles {
                        scopes.extend(role_scopes.iter().cloned());
                    }
                    UserProjectInfo {
                        project_id: format!("gid://conduit/Project/{project_id}").into(),
                        is_owner: project_is_owner,
                        scopes: scopes.into_iter().collect(),
                        roles,
                    }
                },
            )
            .collect();

        let oidc_identities = oidc_rows
            .into_iter()
            .map(
                |(id, idp_name, issuer, subject, identity_email)| OidcIdentityInfo {
                    id: format!("gid://conduit/OIDCIdentity/{id}").into(),
                    idp_name: idp_name.unwrap_or_default(),
                    issuer,
                    subject,
                    email: identity_email.unwrap_or_default(),
                },
            )
            .collect();

        Ok(UserInfo {
            id: format!("gid://conduit/User/{user_id}").into(),
            email,
            first_name,
            last_name,
            is_owner,
            prefer_language,
            avatar: avatar.filter(|value| !value.is_empty()),
            scopes: global_scopes.into_iter().collect(),
            roles: global_roles,
            projects,
            oidc_identities,
            has_password: password != conduit_services::user_service::OIDC_ONLY_PLACEHOLDER,
        })
    }

    async fn my_projects(&self, user_id: i64) -> Result<Vec<Project>, MeError> {
        let rows = sqlx::query_as::<_, ProjectRow>(
            "SELECT CAST(p.id AS TEXT) AS id, p.name, p.status, p.description, \
                    COALESCE(p.profiles, '{}'::jsonb) AS profiles, \
                    p.created_at, p.updated_at, \
                    NULL::timestamptz AS deleted_at \
             FROM projects p \
             JOIN user_projects up ON up.project_id = p.id \
             WHERE up.user_id = $1 AND p.status = 'active' AND p.deleted_at = 0 \
             ORDER BY p.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| MeError::MyProjects(error.to_string()))?;

        Ok(rows
            .into_iter()
            .map(crate::wiring_postgres_project_role::project_row_to_gql)
            .collect())
    }
}

/// PostgreSQL implementation of the current user's profile, password and OIDC
/// identity mutations.
#[derive(Debug, Clone)]
pub(crate) struct PgMeMutationAdapter {
    pool: PgPool,
}

impl PgMeMutationAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn load_gql_user(&self, user_id: i64) -> Result<User, String> {
        let row = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                String,
                String,
                Option<String>,
                bool,
                Json<Vec<String>>,
                chrono::DateTime<chrono::Utc>,
                chrono::DateTime<chrono::Utc>,
            ),
        >(
            "SELECT email, status, prefer_language, first_name, last_name, \
                    avatar, is_owner, COALESCE(scopes, '[]'::jsonb), \
                    created_at, updated_at \
             FROM users WHERE id = $1 AND deleted_at = 0",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "user not found".to_string())?;

        let (
            email,
            status,
            prefer_language,
            first_name,
            last_name,
            avatar,
            is_owner,
            Json(scopes),
            created_at,
            updated_at,
        ) = row;
        Ok(User {
            id: format!("gid://conduit/User/{user_id}").into(),
            created_at: TimeScalar(created_at),
            updated_at: TimeScalar(updated_at),
            email,
            status: if status == "deactivated" {
                UserStatus::Deactivated
            } else {
                UserStatus::Activated
            },
            prefer_language,
            first_name,
            last_name,
            avatar,
            is_owner,
            scopes: Some(scopes),
        })
    }

    async fn stored_password(&self, user_id: i64) -> Result<String, String> {
        sqlx::query_scalar::<_, String>(
            "SELECT password FROM users WHERE id = $1 AND deleted_at = 0",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "user not found".to_string())
    }
}

#[async_trait]
impl conduit_admin_graphql::me_ext::MeMutationServices for PgMeMutationAdapter {
    async fn update_me(&self, user_id: i64, input: UpdateMeInput) -> Result<User, MeMutationError> {
        sqlx::query(
            "UPDATE users SET \
                first_name = COALESCE($1, first_name), \
                last_name = COALESCE($2, last_name), \
                prefer_language = COALESCE($3, prefer_language), \
                avatar = COALESCE($4, avatar), \
                updated_at = now() \
             WHERE id = $5 AND deleted_at = 0",
        )
        .bind(&input.first_name)
        .bind(&input.last_name)
        .bind(&input.prefer_language)
        .bind(&input.avatar)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|error| MeMutationError::UpdateUser(error.to_string()))?;

        self.load_gql_user(user_id)
            .await
            .map_err(MeMutationError::UpdateUser)
    }

    async fn update_my_password(
        &self,
        user_id: i64,
        old_password: String,
        new_password: String,
    ) -> Result<(), MeMutationError> {
        let stored = self
            .stored_password(user_id)
            .await
            .map_err(MeMutationError::UpdatePassword)?;
        if stored != conduit_services::user_service::OIDC_ONLY_PLACEHOLDER {
            if old_password.is_empty() {
                return Err(MeMutationError::UpdatePassword(
                    "current password is required".to_string(),
                ));
            }
            let valid = conduit_auth::password::verify_password_bcrypt_hex(&old_password, &stored)
                .unwrap_or(false);
            if !valid {
                return Err(MeMutationError::UpdatePassword(
                    "incorrect old password".to_string(),
                ));
            }
        }

        let hashed = conduit_auth::password::encode_password_bcrypt_hex(
            &new_password,
            conduit_auth::password::DEFAULT_BCRYPT_COST,
        )
        .map_err(|error| MeMutationError::UpdatePassword(error.to_string()))?;
        sqlx::query(
            "UPDATE users SET password = $1, updated_at = now() \
             WHERE id = $2 AND deleted_at = 0",
        )
        .bind(hashed)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|error| MeMutationError::UpdatePassword(error.to_string()))?;
        Ok(())
    }

    async fn unlink_oidc_identity(
        &self,
        user_id: i64,
        identity_id: String,
    ) -> Result<(), MeMutationError> {
        let guid = parse_guid(&identity_id).map_err(|error| {
            MeMutationError::UnlinkIdentity(format!("failed to get identity: {error}"))
        })?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| MeMutationError::UnlinkIdentity(error.to_string()))?;

        let owner_id = sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM oidc_identities \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(guid.id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| {
            MeMutationError::UnlinkIdentity(format!("failed to get identity: {error}"))
        })?
        .ok_or_else(|| {
            MeMutationError::UnlinkIdentity("failed to get identity: not found".to_string())
        })?;
        if owner_id != user_id {
            return Err(MeMutationError::UnlinkIdentity(
                "permission denied: this identity does not belong to you".to_string(),
            ));
        }

        let stored = sqlx::query_scalar::<_, String>(
            "SELECT password FROM users \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| MeMutationError::UnlinkIdentity(error.to_string()))?
        .ok_or_else(|| MeMutationError::UnlinkIdentity("user not found".to_string()))?;
        if stored == conduit_services::user_service::OIDC_ONLY_PLACEHOLDER {
            let count = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM oidc_identities \
                 WHERE user_id = $1 AND deleted_at = 0",
            )
            .bind(user_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| {
                MeMutationError::UnlinkIdentity(format!("failed to count identities: {error}"))
            })?;
            if count <= 1 {
                return Err(MeMutationError::UnlinkIdentity(
                    "please set a local password before unlinking your last OIDC identity"
                        .to_string(),
                ));
            }
        }

        sqlx::query("DELETE FROM oidc_identities WHERE id = $1 AND user_id = $2")
            .bind(guid.id)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| MeMutationError::UnlinkIdentity(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| MeMutationError::UnlinkIdentity(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::me::MeServices as _;
    use conduit_admin_graphql::me_ext::MeMutationServices as _;

    #[tokio::test]
    async fn postgres_me_queries_and_mutations_work_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;

        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let email = format!("pg-me-{suffix}@example.com");
        let project_name = format!("PG Me {suffix}");
        let system_role_name = format!("PG System {suffix}");
        let project_role_name = format!("PG Project {suffix}");
        let issuer = format!("https://idp-{suffix}.example.com");
        let subject = format!("subject-{suffix}");

        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users \
                (email, status, prefer_language, password, first_name, last_name, \
                 avatar, is_owner, scopes) \
             VALUES ($1, 'activated', 'en', $2, 'Before', 'User', NULL, TRUE, $3) \
             RETURNING id",
        )
        .bind(&email)
        .bind(conduit_services::user_service::OIDC_ONLY_PLACEHOLDER)
        .bind(Json(vec!["direct_scope".to_string()]))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, description, status, profiles) \
             VALUES ($1, 'identity integration', 'active', $2) RETURNING id",
        )
        .bind(&project_name)
        .bind(Json(serde_json::json!({
            "activeProfile": "primary",
            "profiles": [{"name": "primary", "channelIDs": [7]}]
        })))
        .fetch_one(&pool)
        .await?;
        let system_role_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO roles (name, level, project_id, scopes) \
             VALUES ($1, 'system', NULL, $2) RETURNING id",
        )
        .bind(&system_role_name)
        .bind(Json(vec!["system_scope".to_string()]))
        .fetch_one(&pool)
        .await?;
        let project_role_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO roles (name, level, project_id, scopes) \
             VALUES ($1, 'project', $2, $3) RETURNING id",
        )
        .bind(&project_role_name)
        .bind(project_id)
        .bind(Json(vec!["role_scope".to_string()]))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects (user_id, project_id, is_owner, scopes) \
             VALUES ($1, $2, TRUE, $3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(vec!["membership_scope".to_string()]))
        .execute(&pool)
        .await?;
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2), ($1, $3)")
            .bind(user_id)
            .bind(system_role_id)
            .bind(project_role_id)
            .execute(&pool)
            .await?;
        let identity_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO oidc_identities \
                (issuer, subject, email, idp_name, user_id) \
             VALUES ($1, $2, $3, 'Test IdP', $4) RETURNING id",
        )
        .bind(&issuer)
        .bind(&subject)
        .bind(&email)
        .bind(user_id)
        .fetch_one(&pool)
        .await?;

        let queries = PgMeServiceAdapter::new(pool.clone());
        let mutations = PgMeMutationAdapter::new(pool.clone());
        let me = queries.me(user_id).await?;
        assert_eq!(me.email, email);
        assert!(me.is_owner);
        assert!(!me.has_password);
        assert_eq!(me.scopes, vec!["direct_scope", "system_scope"]);
        assert!(me.roles.iter().any(|role| role.name == system_role_name));
        assert_eq!(me.projects.len(), 1);
        assert_eq!(
            me.projects[0].scopes,
            vec!["membership_scope", "role_scope"]
        );
        assert!(
            me.projects[0]
                .roles
                .iter()
                .any(|role| role.name == project_role_name)
        );
        assert_eq!(me.oidc_identities.len(), 1);

        let projects = queries.my_projects(user_id).await?;
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, project_name);
        assert!(
            projects[0]
                .profiles
                .as_ref()
                .is_some_and(|profiles| profiles.active_profile == "primary")
        );

        let updated = mutations
            .update_me(
                user_id,
                UpdateMeInput {
                    first_name: Some("After".to_string()),
                    last_name: None,
                    prefer_language: Some("zh-CN".to_string()),
                    avatar: Some("https://example.com/avatar.png".to_string()),
                },
            )
            .await?;
        assert_eq!(updated.first_name, "After");
        assert_eq!(updated.last_name, "User");
        assert_eq!(updated.prefer_language, "zh-CN");

        let last_identity_guard = mutations
            .unlink_oidc_identity(user_id, format!("gid://conduit/OIDCIdentity/{identity_id}"))
            .await;
        assert!(matches!(
            last_identity_guard,
            Err(MeMutationError::UnlinkIdentity(message))
                if message.contains("set a local password")
        ));

        mutations
            .update_my_password(user_id, String::new(), "first-password".to_string())
            .await?;
        let stored = sqlx::query_scalar::<_, String>("SELECT password FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await?;
        assert!(conduit_auth::password::verify_password_bcrypt_hex(
            "first-password",
            &stored
        )?);
        let wrong_old = mutations
            .update_my_password(
                user_id,
                "wrong-password".to_string(),
                "second-password".to_string(),
            )
            .await;
        assert!(matches!(
            wrong_old,
            Err(MeMutationError::UpdatePassword(message))
                if message.contains("incorrect old password")
        ));
        mutations
            .update_my_password(
                user_id,
                "first-password".to_string(),
                "second-password".to_string(),
            )
            .await?;
        mutations
            .unlink_oidc_identity(user_id, format!("gid://conduit/OIDCIdentity/{identity_id}"))
            .await?;
        let remaining_identities =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM oidc_identities WHERE user_id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining_identities, 0);

        sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_projects WHERE user_id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM roles WHERE id = $1 OR id = $2")
            .bind(system_role_id)
            .bind(project_role_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        Ok(())
    }
}

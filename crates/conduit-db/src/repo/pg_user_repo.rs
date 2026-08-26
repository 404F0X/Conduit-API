//! PostgreSQL-backed user repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::user_repo::{CreateUserInput, ListUsersQuery, ListUsersResult, UpdateUserInput};
use crate::repo::{RepoError, RepoResult, RequestContext, UserRepo};
use crate::row::UserRow;

const USER_SELECT_COLUMNS: &str = "\
CAST(id AS TEXT) AS id, email, status, prefer_language, first_name, last_name, avatar, \
is_owner, scopes, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgUserRepo {
    pool: PgPool,
}

impl PgUserRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn parse_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("user id not a valid integer"))
}

fn parse_dt(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .map(|value| DateTime::from_naive_utc_and_offset(value, Utc))
        })
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

fn database_error(context: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres user repo {context} failed: {error}"))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
}

#[async_trait]
impl UserRepo for PgUserRepo {
    async fn create_user_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateUserInput,
    ) -> RepoResult<UserRow> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| database_error("begin", e))?;
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at = 0)",
        )
        .bind(&input.email)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| database_error("create pre-check", e))?;
        if exists {
            return Err(RepoError::EmailConflict);
        }
        let result = sqlx::query(
            "INSERT INTO users (email, status, prefer_language, password, first_name, last_name, \
             avatar, is_owner, scopes) VALUES ($1, 'activated', $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&input.email)
        .bind(input.prefer_language.as_deref().unwrap_or("en"))
        .bind(&input.password_hash)
        .bind(input.first_name.as_deref().unwrap_or_default())
        .bind(input.last_name.as_deref().unwrap_or_default())
        .bind(&input.avatar)
        .bind(input.is_owner)
        .bind(sqlx::types::Json(&input.scopes))
        .execute(&mut *tx)
        .await;
        if let Err(error) = result {
            if is_unique_violation(&error) {
                return Err(RepoError::EmailConflict);
            }
            return Err(database_error("create insert", error));
        }
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE email = $1 AND deleted_at = 0"
        ))
        .bind(&input.email)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| database_error("create readback", e))?;
        tx.commit().await.map_err(|e| database_error("commit", e))?;
        Ok(row)
    }

    async fn find_user_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(parse_id(user_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database_error("find by id", e))
    }

    async fn find_user_by_id_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE id = $1"
        ))
        .bind(parse_id(user_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database_error("find by id with deleted", e))
    }

    async fn find_user_by_email_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE email = $1 AND deleted_at = 0"
        ))
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database_error("find by email", e))
    }

    async fn find_user_by_email_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE email = $1 \
             ORDER BY deleted_at ASC LIMIT 1"
        ))
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| database_error("find by email with deleted", e))
    }

    async fn list_users_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListUsersQuery,
    ) -> RepoResult<ListUsersResult> {
        let fetch_n = i64::from(query.limit).saturating_add(1);
        let rows = if let (Some(timestamp), Some(id)) =
            (query.after_created_at.as_deref(), query.after_id.as_deref())
        {
            sqlx::query_as::<_, UserRow>(&format!(
                "SELECT {USER_SELECT_COLUMNS} FROM users WHERE deleted_at = 0 \
                 AND (created_at > $1 OR (created_at = $1 AND id > $2)) \
                 ORDER BY created_at, id LIMIT $3"
            ))
            .bind(parse_dt(timestamp))
            .bind(parse_id(id)?)
            .bind(fetch_n)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| database_error("list keyset", e))?
        } else {
            sqlx::query_as::<_, UserRow>(&format!(
                "SELECT {USER_SELECT_COLUMNS} FROM users WHERE deleted_at = 0 \
                 ORDER BY created_at, id LIMIT $1 OFFSET $2"
            ))
            .bind(fetch_n)
            .bind(i64::from(query.offset))
            .fetch_all(&self.pool)
            .await
            .map_err(|e| database_error("list offset", e))?
        };
        let has_more = rows.len() > query.limit as usize;
        let mut rows = rows;
        rows.truncate(query.limit as usize);
        Ok(ListUsersResult { rows, has_more })
    }

    async fn update_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        input: UpdateUserInput,
    ) -> RepoResult<UserRow> {
        let id = parse_id(user_id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| database_error("begin", e))?;
        if let Some(email) = &input.email {
            let conflict: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at = 0 AND id <> $2)",
            )
            .bind(email)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| database_error("update pre-check", e))?;
            if conflict {
                return Err(RepoError::EmailConflict);
            }
        }
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE users SET ");
        let mut set = builder.separated(", ");
        if let Some(value) = input.email {
            set.push("email = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.first_name {
            set.push("first_name = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.last_name {
            set.push("last_name = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.prefer_language {
            set.push("prefer_language = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.avatar {
            set.push("avatar = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.is_owner {
            set.push("is_owner = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.scopes {
            set.push("scopes = ")
                .push_bind_unseparated(sqlx::types::Json(value));
        }
        if let Some(value) = input.status {
            set.push("status = ").push_bind_unseparated(value);
        }
        set.push("updated_at = ")
            .push_bind_unseparated(parse_dt(&input.updated_at));
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(id)
            .push(" AND deleted_at = 0");
        let affected = builder
            .build()
            .execute(&mut *tx)
            .await
            .map_err(|e| database_error("update", e))?
            .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound("user"));
        }
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| database_error("update readback", e))?;
        tx.commit().await.map_err(|e| database_error("commit", e))?;
        Ok(row)
    }

    async fn soft_delete_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        _deleted_at: &str,
    ) -> RepoResult<UserRow> {
        let id = parse_id(user_id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| database_error("begin", e))?;
        let email: Option<String> = sqlx::query_scalar(
            "SELECT email FROM users WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| database_error("delete lookup", e))?;
        let email = email.ok_or(RepoError::NotFound("user"))?;
        let max_deleted: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(deleted_at), 0) FROM users WHERE email = $1")
                .bind(email)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| database_error("delete max", e))?;
        let deleted_at = Utc::now().timestamp().max(max_deleted.saturating_add(1));
        sqlx::query("UPDATE users SET deleted_at = $1, status = 'deactivated', updated_at = now() WHERE id = $2")
            .bind(deleted_at).bind(id).execute(&mut *tx).await.map_err(|e| database_error("delete", e))?;
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| database_error("delete readback", e))?;
        tx.commit().await.map_err(|e| database_error("commit", e))?;
        Ok(row)
    }

    async fn restore_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<UserRow> {
        let id = parse_id(user_id)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| database_error("begin", e))?;
        let snapshot: Option<(String, i64)> =
            sqlx::query_as("SELECT email, deleted_at FROM users WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| database_error("restore lookup", e))?;
        let (email, deleted_at) = snapshot.ok_or(RepoError::NotFound("user"))?;
        if deleted_at != 0 {
            let conflict: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at = 0 AND id <> $2)")
                .bind(email).bind(id).fetch_one(&mut *tx).await.map_err(|e| database_error("restore pre-check", e))?;
            if conflict {
                return Err(RepoError::EmailConflict);
            }
            sqlx::query("UPDATE users SET deleted_at = 0, status = 'activated', updated_at = now() WHERE id = $1")
                .bind(id).execute(&mut *tx).await.map_err(|e| database_error("restore", e))?;
        }
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_SELECT_COLUMNS} FROM users WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| database_error("restore readback", e))?;
        tx.commit().await.map_err(|e| database_error("commit", e))?;
        Ok(row)
    }

    async fn user_exists_unchecked(&self, _ctx: &RequestContext, email: &str) -> RepoResult<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND deleted_at = 0)")
            .bind(email)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| database_error("exists", e))
    }

    async fn user_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)")
            .bind(email)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| database_error("exists with deleted", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyContext, Principal};

    #[tokio::test]
    async fn live_postgres_user_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgUserRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let email = format!("pg-user-{}@example.test", std::process::id());
        sqlx::query("DELETE FROM users WHERE email = $1")
            .bind(&email)
            .execute(&pool)
            .await?;
        let created = repo
            .create_user(
                &ctx,
                CreateUserInput {
                    id: "ignored".into(),
                    email: email.clone(),
                    password_hash: "hash".into(),
                    first_name: Some("Postgres".into()),
                    last_name: None,
                    prefer_language: Some("zh-CN".into()),
                    avatar: None,
                    is_owner: false,
                    scopes: vec!["read_projects".into()],
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .await?;
        assert_eq!(created.email, email);
        assert_eq!(created.scopes, vec!["read_projects"]);
        let updated = repo
            .update_user(
                &ctx,
                &created.id,
                UpdateUserInput {
                    first_name: Some("Updated".into()),
                    updated_at: Utc::now().to_rfc3339(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.first_name, "Updated");
        assert!(
            repo.soft_delete_user(&ctx, &created.id, "ignored")
                .await?
                .deleted_at
                .is_some()
        );
        assert!(repo.find_user_by_email(&ctx, &email).await?.is_none());
        assert!(
            repo.restore_user(&ctx, &created.id)
                .await?
                .deleted_at
                .is_none()
        );
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(created.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}

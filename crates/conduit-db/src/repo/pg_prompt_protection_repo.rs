//! PostgreSQL-backed prompt-protection-rule repository.
//!
//! Prompt-protection rules are global: the Go Ent schema and the PostgreSQL
//! migration contain no `project_id` column. The legacy [`PromptProtectionRepo`]
//! argument is therefore used only by its checked policy wrapper; storage
//! operations intentionally ignore it.

use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::prompt_protection_repo::{
    CreateProtectionRuleInput, PromptProtectionRuleRepo, RULE_STATUS_ENABLED,
    UpdateProtectionRuleInput,
};
use crate::repo::{PromptProtectionRepo, RepoError, RepoResult, RequestContext};
use crate::row::PromptProtectionRuleRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, name, description, pattern, status, settings, \
created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgPromptProtectionRuleRepo {
    pool: PgPool,
}

impl PgPromptProtectionRuleRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn fetch(
        &self,
        rule_id: i64,
        live_only: bool,
    ) -> RepoResult<Option<PromptProtectionRuleRow>> {
        let live_filter = if live_only { " AND deleted_at = 0" } else { "" };
        sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
            "SELECT {COLUMNS} FROM prompt_protection_rules \
             WHERE id = $1{live_filter}"
        ))
        .bind(rule_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("fetch", error))
    }

    async fn list_where_status(
        &self,
        status: Option<&str>,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        match status {
            Some(status) => sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
                "SELECT {COLUMNS} FROM prompt_protection_rules \
                 WHERE deleted_at = 0 AND status = $1 ORDER BY id ASC"
            ))
            .bind(status)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error("list by status", error)),
            None => sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
                "SELECT {COLUMNS} FROM prompt_protection_rules \
                 WHERE deleted_at = 0 ORDER BY id ASC"
            ))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error("list", error)),
        }
    }

    fn parse_bulk_ids(rule_ids: &[String]) -> Vec<i64> {
        rule_ids
            .iter()
            .filter_map(|rule_id| rule_id.parse::<i64>().ok())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

fn parse_id(value: &str) -> RepoResult<i64> {
    value
        .parse::<i64>()
        .map_err(|_| RepoError::NotFound("prompt protection rule id not a valid integer"))
}

fn parse_timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .map(|timestamp| DateTime::from_naive_utc_and_offset(timestamp, Utc))
        })
        .or_else(|_| {
            NaiveDate::parse_from_str(value, "%Y-%m-%d").map(|date| {
                DateTime::from_naive_utc_and_offset(
                    date.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    Utc,
                )
            })
        })
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

fn database_error(context: &str, error: sqlx::Error) -> RepoError {
    if error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!(
            "postgres prompt protection repo {context} failed: {error}"
        ))
    }
}

fn next_deleted_at(maximum: i64) -> i64 {
    Utc::now().timestamp().max(1).max(maximum.saturating_add(1))
}

#[async_trait]
impl PromptProtectionRuleRepo for PgPromptProtectionRuleRepo {
    async fn create_protection_rule_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow> {
        let created_at = parse_timestamp(&input.created_at);
        sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
            "INSERT INTO prompt_protection_rules \
             (name, description, pattern, settings, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $5) RETURNING {COLUMNS}"
        ))
        .bind(input.name)
        .bind(input.description.unwrap_or_default())
        .bind(input.pattern)
        .bind(sqlx::types::Json(input.settings))
        .bind(created_at)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| database_error("create", error))
    }

    async fn find_protection_rule_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>> {
        let Ok(rule_id) = rule_id.parse::<i64>() else {
            return Ok(None);
        };
        self.fetch(rule_id, true).await
    }

    async fn find_protection_rule_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<Option<PromptProtectionRuleRow>> {
        let Ok(rule_id) = rule_id.parse::<i64>() else {
            return Ok(None);
        };
        self.fetch(rule_id, false).await
    }

    async fn list_protection_rules_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        self.list_where_status(None).await
    }

    async fn list_enabled_protection_rules_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        self.list_where_status(Some(RULE_STATUS_ENABLED)).await
    }

    async fn update_protection_rule_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_id: &str,
        input: UpdateProtectionRuleInput,
    ) -> RepoResult<PromptProtectionRuleRow> {
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE prompt_protection_rules SET ");
        {
            let mut fields = builder.separated(", ");
            if let Some(value) = input.name {
                fields.push("name = ").push_bind_unseparated(value);
            }
            if let Some(value) = input.description {
                fields.push("description = ").push_bind_unseparated(value);
            }
            if let Some(value) = input.pattern {
                fields.push("pattern = ").push_bind_unseparated(value);
            }
            if let Some(value) = input.status {
                fields.push("status = ").push_bind_unseparated(value);
            }
            if let Some(value) = input.settings {
                fields
                    .push("settings = ")
                    .push_bind_unseparated(sqlx::types::Json(value));
            }
            fields
                .push("updated_at = ")
                .push_bind_unseparated(parse_timestamp(&input.updated_at));
        }
        builder
            .push(" WHERE id = ")
            .push_bind(parse_id(rule_id)?)
            .push(" AND deleted_at = 0 RETURNING ")
            .push(COLUMNS);

        builder
            .build_query_as::<PromptProtectionRuleRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error("update", error))?
            .ok_or(RepoError::NotFound("prompt protection rule"))
    }

    async fn set_protection_rule_status_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_id: &str,
        status: &str,
        updated_at: String,
    ) -> RepoResult<PromptProtectionRuleRow> {
        sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
            "UPDATE prompt_protection_rules SET status = $1, updated_at = $2 \
             WHERE id = $3 AND deleted_at = 0 RETURNING {COLUMNS}"
        ))
        .bind(status)
        .bind(parse_timestamp(&updated_at))
        .bind(parse_id(rule_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("set status", error))?
        .ok_or(RepoError::NotFound("prompt protection rule"))
    }

    async fn soft_delete_protection_rule_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_id: &str,
    ) -> RepoResult<PromptProtectionRuleRow> {
        let rule_id = parse_id(rule_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| database_error("soft delete begin", error))?;

        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM prompt_protection_rules \
             WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(rule_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete lock", error))?
        .ok_or(RepoError::NotFound("prompt protection rule"))?;

        let maximum = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(deleted_at), 0) FROM prompt_protection_rules WHERE name = $1",
        )
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete timestamp lookup", error))?;

        let row = sqlx::query_as::<_, PromptProtectionRuleRow>(&format!(
            "UPDATE prompt_protection_rules \
             SET deleted_at = $1, updated_at = now() \
             WHERE id = $2 AND deleted_at = 0 RETURNING {COLUMNS}"
        ))
        .bind(next_deleted_at(maximum))
        .bind(rule_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete", error))?
        .ok_or(RepoError::NotFound("prompt protection rule"))?;

        transaction
            .commit()
            .await
            .map_err(|error| database_error("soft delete commit", error))?;
        Ok(row)
    }

    async fn bulk_delete_protection_rules_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_ids: &[String],
    ) -> RepoResult<u64> {
        let rule_ids = Self::parse_bulk_ids(rule_ids);
        if rule_ids.is_empty() {
            return Ok(0);
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| database_error("bulk delete begin", error))?;
        let mut affected = 0;

        for rule_id in rule_ids {
            let name = sqlx::query_scalar::<_, String>(
                "SELECT name FROM prompt_protection_rules \
                 WHERE id = $1 AND deleted_at = 0 FOR UPDATE",
            )
            .bind(rule_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| database_error("bulk delete lock", error))?;
            let Some(name) = name else {
                continue;
            };

            let maximum = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(deleted_at), 0) \
                 FROM prompt_protection_rules WHERE name = $1",
            )
            .bind(name)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| database_error("bulk delete timestamp lookup", error))?;

            affected += sqlx::query(
                "UPDATE prompt_protection_rules \
                 SET deleted_at = $1, updated_at = now() \
                 WHERE id = $2 AND deleted_at = 0",
            )
            .bind(next_deleted_at(maximum))
            .bind(rule_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| database_error("bulk delete", error))?
            .rows_affected();
        }

        transaction
            .commit()
            .await
            .map_err(|error| database_error("bulk delete commit", error))?;
        Ok(affected)
    }

    async fn bulk_set_protection_rule_status_unchecked(
        &self,
        _ctx: &RequestContext,
        rule_ids: &[String],
        status: &str,
    ) -> RepoResult<u64> {
        let rule_ids = Self::parse_bulk_ids(rule_ids);
        if rule_ids.is_empty() {
            return Ok(0);
        }

        let mut builder =
            QueryBuilder::<Postgres>::new("UPDATE prompt_protection_rules SET status = ");
        builder
            .push_bind(status)
            .push(", updated_at = now() WHERE deleted_at = 0 AND id IN (");
        {
            let mut ids = builder.separated(", ");
            for rule_id in rule_ids {
                ids.push_bind(rule_id);
            }
        }
        builder.push(")");

        builder
            .build()
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(|error| database_error("bulk set status", error))
    }
}

#[async_trait]
impl PromptProtectionRepo for PgPromptProtectionRuleRepo {
    async fn list_prompt_rules_unchecked(
        &self,
        _ctx: &RequestContext,
        _project_id: &str,
    ) -> RepoResult<Vec<PromptProtectionRuleRow>> {
        self.list_where_status(None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::policy::{PolicyContext, Principal};
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;

    struct IsolatedPostgres {
        pool: PgPool,
        admin_pool: PgPool,
        schema: String,
    }

    impl IsolatedPostgres {
        async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let admin_pool = PgPool::connect(dsn).await?;
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
            let schema = format!("conduit_prompt_protection_{}_{}", std::process::id(), nonce);
            sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
                .execute(&admin_pool)
                .await?;

            let search_path = format!("SET search_path TO \"{schema}\"");
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .after_connect(move |connection, _| {
                    let search_path = search_path.clone();
                    Box::pin(async move {
                        sqlx::query(&search_path).execute(connection).await?;
                        Ok(())
                    })
                })
                .connect(dsn)
                .await?;
            crate::connection::migrate_postgres_with_flag(&pool, false).await?;

            Ok(Self {
                pool,
                admin_pool,
                schema,
            })
        }

        async fn cleanup(self) -> Result<(), sqlx::Error> {
            self.pool.close().await;
            sqlx::query(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
                .execute(&self.admin_pool)
                .await?;
            self.admin_pool.close().await;
            Ok(())
        }
    }

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn settings(action: &str) -> Value {
        json!({"action": action, "replacement": "[MASKED]", "scopes": ["user"]})
    }

    fn input(name: &str) -> CreateProtectionRuleInput {
        CreateProtectionRuleInput {
            name: name.into(),
            description: Some("description".into()),
            pattern: "(?i)secret-[0-9]+".into(),
            settings: settings("mask"),
            created_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn postgres_prompt_protection_repo_isolated_crud_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = IsolatedPostgres::new(&dsn).await?;
        let repo = PgPromptProtectionRuleRepo::new(database.pool.clone());
        let ctx = context();

        let first = repo.create_protection_rule(&ctx, input("shared")).await?;
        assert_eq!(first.status, "disabled");
        assert_eq!(first.settings, settings("mask"));
        assert_eq!(first.created_at, parse_timestamp("2024-01-01T00:00:00Z"));
        assert!(
            repo.find_protection_rule(&ctx, "not-an-id")
                .await?
                .is_none()
        );
        assert!(matches!(
            repo.create_protection_rule(&ctx, input("shared")).await,
            Err(RepoError::NameConflict)
        ));

        let second = repo.create_protection_rule(&ctx, input("second")).await?;
        let enabled = repo
            .set_protection_rule_status(
                &ctx,
                &second.id,
                RULE_STATUS_ENABLED,
                "2024-02-01T00:00:00Z".into(),
            )
            .await?;
        assert_eq!(enabled.status, RULE_STATUS_ENABLED);
        assert_eq!(
            repo.list_enabled_protection_rules(&ctx)
                .await?
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec![second.id.as_str()]
        );

        let updated = repo
            .update_protection_rule(
                &ctx,
                &first.id,
                UpdateProtectionRuleInput {
                    name: Some("renamed".into()),
                    description: Some("updated".into()),
                    pattern: Some("token-[a-z]+".into()),
                    settings: Some(settings("reject")),
                    updated_at: "2024-03-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.description, "updated");
        assert_eq!(updated.settings, settings("reject"));
        assert!(matches!(
            repo.update_protection_rule(
                &ctx,
                &updated.id,
                UpdateProtectionRuleInput {
                    name: Some("second".into()),
                    updated_at: "2024-04-01T00:00:00Z".into(),
                    ..Default::default()
                }
            )
            .await,
            Err(RepoError::NameConflict)
        ));

        let deleted = repo.soft_delete_protection_rule(&ctx, &updated.id).await?;
        assert!(deleted.deleted_at.is_some());
        assert!(
            repo.find_protection_rule(&ctx, &updated.id)
                .await?
                .is_none()
        );
        assert!(
            repo.find_protection_rule_with_deleted(&ctx, &updated.id)
                .await?
                .is_some()
        );

        // Repeated delete/recreate cycles must not collide on
        // `(name, deleted_at)`, even when they happen in the same second.
        let replacement = repo.create_protection_rule(&ctx, input("renamed")).await?;
        repo.soft_delete_protection_rule(&ctx, &replacement.id)
            .await?;
        let final_replacement = repo.create_protection_rule(&ctx, input("renamed")).await?;

        assert_eq!(
            repo.bulk_set_protection_rule_status(
                &ctx,
                &[final_replacement.id.clone(), "999999".into(), "bad".into()],
                RULE_STATUS_ENABLED,
            )
            .await?,
            1
        );
        assert_eq!(
            repo.bulk_delete_protection_rules(
                &ctx,
                &[
                    second.id.clone(),
                    final_replacement.id.clone(),
                    "bad".into()
                ],
            )
            .await?,
            2
        );
        assert!(repo.list_protection_rules(&ctx).await?.is_empty());
        assert!(
            PromptProtectionRepo::list_prompt_rules(&repo, &ctx, "1")
                .await?
                .is_empty()
        );

        let anonymous = RequestContext::new(PolicyContext::anonymous());
        assert!(matches!(
            repo.list_protection_rules(&anonymous).await,
            Err(RepoError::Policy(_))
        ));

        database.cleanup().await?;
        Ok(())
    }
}

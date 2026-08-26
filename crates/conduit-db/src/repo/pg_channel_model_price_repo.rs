//! PostgreSQL-backed channel procurement-price head and history repository.
use crate::repo::channel_model_price_repo::{
    ChannelModelPriceRepo, VERSION_STATUS_ACTIVE, VERSION_STATUS_ARCHIVED,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::{ChannelModelPriceRow, ChannelModelPriceVersionRow};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
const HEAD: &str = "CAST(id AS TEXT) AS id,CAST(channel_id AS TEXT) AS channel_id,model_id,currency_code,price,reference_id,created_at,updated_at,CASE WHEN deleted_at=0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";
const VERSION: &str = "CAST(id AS TEXT) AS id,CAST(channel_id AS TEXT) AS channel_id,model_id,CAST(channel_model_price_id AS TEXT) AS channel_model_price_id,currency_code,price,status,effective_start_at,effective_end_at,reference_id,created_at,updated_at";
#[derive(Debug, Clone)]
pub struct PgChannelModelPriceRepo {
    pool: PgPool,
}
impl PgChannelModelPriceRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn list_prices_by_channel_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        channel_id: i64,
    ) -> RepoResult<Vec<ChannelModelPriceRow>> {
        sqlx::query_as::<_, ChannelModelPriceRow>(&format!(
            "SELECT {HEAD} FROM channel_model_prices \
             WHERE channel_id=$1 AND deleted_at=0 ORDER BY id"
        ))
        .bind(channel_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| err("list in transaction", e))
    }

    pub async fn create_price_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        channel_id: i64,
        model_id: &str,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_model_prices \
             (channel_id,model_id,currency_code,price,reference_id,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$6) RETURNING id",
        )
        .bind(channel_id)
        .bind(model_id)
        .bind(currency_code)
        .bind(sqlx::types::Json(price))
        .bind(reference_id)
        .bind(ts(now))
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| err("create in transaction", e))?;
        Self::head_in_tx(tx, id)
            .await?
            .ok_or(RepoError::NotFound("channel model price"))
    }

    pub async fn update_price_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: i64,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        let updated = sqlx::query(
            "UPDATE channel_model_prices SET currency_code=$2,price=$3,reference_id=$4,updated_at=$5 \
             WHERE id=$1 AND deleted_at=0",
        )
        .bind(id)
        .bind(currency_code)
        .bind(sqlx::types::Json(price))
        .bind(reference_id)
        .bind(ts(now))
        .execute(&mut **tx)
        .await
        .map_err(|e| err("update in transaction", e))?;
        if updated.rows_affected() == 0 {
            return Err(RepoError::NotFound("channel model price"));
        }
        Self::head_in_tx(tx, id)
            .await?
            .ok_or(RepoError::NotFound("channel model price"))
    }

    pub async fn soft_delete_price_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: i64,
        now: &str,
    ) -> RepoResult<()> {
        let key = sqlx::query_as::<_, (i64, String)>(
            "SELECT channel_id,model_id FROM channel_model_prices \
             WHERE id=$1 AND deleted_at=0",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| err("delete key in transaction", e))?
        .ok_or(RepoError::NotFound("channel model price"))?;
        let max = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(deleted_at),0) FROM channel_model_prices \
             WHERE channel_id=$1 AND model_id=$2",
        )
        .bind(key.0)
        .bind(key.1)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| err("delete max in transaction", e))?;
        let deleted = ts(now).timestamp().max(1).max(max + 1);
        sqlx::query(
            "UPDATE channel_model_prices SET deleted_at=$2,updated_at=now() \
             WHERE id=$1 AND deleted_at=0",
        )
        .bind(id)
        .bind(deleted)
        .execute(&mut **tx)
        .await
        .map_err(|e| err("delete in transaction", e))?;
        Ok(())
    }

    pub async fn archive_active_versions_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: i64,
        end: &str,
    ) -> RepoResult<u64> {
        sqlx::query(
            "UPDATE channel_model_price_versions \
             SET status=$2,effective_end_at=$3,updated_at=now() \
             WHERE channel_model_price_id=$1 AND status=$4",
        )
        .bind(id)
        .bind(VERSION_STATUS_ARCHIVED)
        .bind(ts(end))
        .bind(VERSION_STATUS_ACTIVE)
        .execute(&mut **tx)
        .await
        .map(|result| result.rows_affected())
        .map_err(|e| err("archive in transaction", e))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_version_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        channel_id: i64,
        model_id: &str,
        head_id: i64,
        currency_code: &str,
        price: &Value,
        status: &str,
        start: &str,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceVersionRow> {
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_model_price_versions \
             (channel_id,model_id,channel_model_price_id,currency_code,price,status, \
              effective_start_at,reference_id,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) RETURNING id",
        )
        .bind(channel_id)
        .bind(model_id)
        .bind(head_id)
        .bind(currency_code)
        .bind(sqlx::types::Json(price))
        .bind(status)
        .bind(ts(start))
        .bind(reference_id)
        .bind(ts(now))
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| err("version in transaction", e))?;
        sqlx::query_as::<_, ChannelModelPriceVersionRow>(&format!(
            "SELECT {VERSION} FROM channel_model_price_versions WHERE id=$1"
        ))
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| err("version readback in transaction", e))
    }

    async fn head_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: i64,
    ) -> RepoResult<Option<ChannelModelPriceRow>> {
        sqlx::query_as::<_, ChannelModelPriceRow>(&format!(
            "SELECT {HEAD} FROM channel_model_prices WHERE id=$1"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| err("head in transaction", e))
    }

    async fn head(&self, id: i64) -> RepoResult<Option<ChannelModelPriceRow>> {
        sqlx::query_as::<_, ChannelModelPriceRow>(&format!(
            "SELECT {HEAD} FROM channel_model_prices WHERE id=$1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| err("head", e))
    }
}
fn ts(v: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(v)
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_default()
}
fn err(c: &str, e: sqlx::Error) -> RepoError {
    if e.as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|v| v == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!("postgres channel model price repo {c} failed: {e}"))
    }
}
#[async_trait]
impl ChannelModelPriceRepo for PgChannelModelPriceRepo {
    async fn list_prices_by_channel_unchecked(
        &self,
        _: &RequestContext,
        channel_id: i64,
    ) -> RepoResult<Vec<ChannelModelPriceRow>> {
        sqlx::query_as::<_,ChannelModelPriceRow>(&format!("SELECT {HEAD} FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0 ORDER BY id")).bind(channel_id).fetch_all(&self.pool).await.map_err(|e|err("list",e))
    }
    async fn create_price_unchecked(
        &self,
        _: &RequestContext,
        channel_id: i64,
        model_id: &str,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO channel_model_prices(channel_id,model_id,currency_code,price,reference_id,created_at,updated_at)VALUES($1,$2,$3,$4,$5,$6,$6)RETURNING id").bind(channel_id).bind(model_id).bind(currency_code).bind(sqlx::types::Json(price)).bind(reference_id).bind(ts(now)).fetch_one(&self.pool).await.map_err(|e|err("create",e))?;
        self.head(id)
            .await?
            .ok_or(RepoError::NotFound("channel model price"))
    }
    async fn update_price_unchecked(
        &self,
        _: &RequestContext,
        id: i64,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        if sqlx::query("UPDATE channel_model_prices SET currency_code=$2,price=$3,reference_id=$4,updated_at=$5 WHERE id=$1 AND deleted_at=0").bind(id).bind(currency_code).bind(sqlx::types::Json(price)).bind(reference_id).bind(ts(now)).execute(&self.pool).await.map_err(|e|err("update",e))?.rows_affected()==0{return Err(RepoError::NotFound("channel model price"))}
        self.head(id)
            .await?
            .ok_or(RepoError::NotFound("channel model price"))
    }
    async fn soft_delete_price_unchecked(
        &self,
        _: &RequestContext,
        id: i64,
        now: &str,
    ) -> RepoResult<()> {
        let key = sqlx::query_as::<_, (i64, String)>(
            "SELECT channel_id,model_id FROM channel_model_prices WHERE id=$1 AND deleted_at=0",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| err("delete key", e))?
        .ok_or(RepoError::NotFound("channel model price"))?;
        let max=sqlx::query_scalar::<_,i64>("SELECT COALESCE(MAX(deleted_at),0) FROM channel_model_prices WHERE channel_id=$1 AND model_id=$2").bind(key.0).bind(key.1).fetch_one(&self.pool).await.map_err(|e|err("delete max",e))?;
        let deleted = ts(now).timestamp().max(1).max(max + 1);
        sqlx::query("UPDATE channel_model_prices SET deleted_at=$2,updated_at=now() WHERE id=$1 AND deleted_at=0").bind(id).bind(deleted).execute(&self.pool).await.map_err(|e|err("delete",e))?;
        Ok(())
    }
    async fn archive_active_versions_unchecked(
        &self,
        _: &RequestContext,
        id: i64,
        end: &str,
    ) -> RepoResult<u64> {
        sqlx::query("UPDATE channel_model_price_versions SET status=$2,effective_end_at=$3,updated_at=now() WHERE channel_model_price_id=$1 AND status=$4").bind(id).bind(VERSION_STATUS_ARCHIVED).bind(ts(end)).bind(VERSION_STATUS_ACTIVE).execute(&self.pool).await.map(|v|v.rows_affected()).map_err(|e|err("archive",e))
    }
    async fn create_version_unchecked(
        &self,
        _: &RequestContext,
        channel_id: i64,
        model_id: &str,
        head_id: i64,
        currency_code: &str,
        price: &Value,
        status: &str,
        start: &str,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceVersionRow> {
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO channel_model_price_versions(channel_id,model_id,channel_model_price_id,currency_code,price,status,effective_start_at,reference_id,created_at,updated_at)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9)RETURNING id").bind(channel_id).bind(model_id).bind(head_id).bind(currency_code).bind(sqlx::types::Json(price)).bind(status).bind(ts(start)).bind(reference_id).bind(ts(now)).fetch_one(&self.pool).await.map_err(|e|err("version",e))?;
        sqlx::query_as::<_, ChannelModelPriceVersionRow>(&format!(
            "SELECT {VERSION} FROM channel_model_price_versions WHERE id=$1"
        ))
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| err("version readback", e))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    #[tokio::test]
    async fn postgres_price_version_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgChannelModelPriceRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let price = serde_json::json!({"items":[{"itemCode":"prompt_tokens","price":0.01}]});
        let head = repo
            .create_price(
                &ctx,
                1,
                "mock-chat",
                "CNY",
                &price,
                "head-v1",
                "2026-08-15T00:00:00Z",
            )
            .await?;
        let version = repo
            .create_version(
                &ctx,
                1,
                "mock-chat",
                head.id.parse()?,
                "CNY",
                &price,
                VERSION_STATUS_ACTIVE,
                "2026-08-15T00:00:00Z",
                "version-v1",
                "2026-08-15T00:00:00Z",
            )
            .await?;
        assert_eq!(version.status, VERSION_STATUS_ACTIVE);
        assert_eq!(version.currency_code, "CNY");
        assert_eq!(
            repo.archive_active_versions(&ctx, head.id.parse()?, "2026-08-16T00:00:00Z")
                .await?,
            1
        );
        let updated = repo
            .update_price(
                &ctx,
                head.id.parse()?,
                "CNY",
                &serde_json::json!({"items":[]}),
                "head-v2",
                "2026-08-16T00:00:00Z",
            )
            .await?;
        assert_eq!(updated.reference_id, "head-v2");
        assert_eq!(updated.currency_code, "CNY");
        database.cleanup().await?;
        Ok(())
    }
}

//! PostgreSQL ownership shared by scheduled, manual and recovery callers.
use sqlx::PgPool;
use std::{future::Future, time::Duration};
use uuid::Uuid;

pub async fn run<T>(
    pool: &PgPool,
    key: &str,
    once: bool,
    work: impl Future<Output = Result<T, String>>,
) -> Result<Option<T>, String> {
    run_with_completion(pool, key, work, |_| once).await
}

pub async fn run_with_completion<T>(
    pool: &PgPool,
    key: &str,
    work: impl Future<Output = Result<T, String>>,
    complete: impl FnOnce(&T) -> bool,
) -> Result<Option<T>, String> {
    let owner = Uuid::new_v4();
    let claimed = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO maintenance_claims(job_key,owner,lease_until) VALUES($1,$2,now()+interval '120 seconds') \
         ON CONFLICT(job_key) DO UPDATE SET owner=EXCLUDED.owner,lease_until=EXCLUDED.lease_until,updated_at=now() \
         WHERE maintenance_claims.lease_until<=now() AND maintenance_claims.completed_at IS NULL RETURNING owner")
        .bind(key).bind(owner).fetch_optional(pool).await.map_err(|e| e.to_string())?;
    if claimed != Some(owner) {
        return Ok(None);
    }
    tokio::pin!(work);
    let mut renewal = tokio::time::interval(Duration::from_secs(30));
    renewal.tick().await;
    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            _ = renewal.tick() => {
                let changed = sqlx::query("UPDATE maintenance_claims SET lease_until=now()+interval '120 seconds',updated_at=now() WHERE job_key=$1 AND owner=$2 AND lease_until>now()")
                    .bind(key).bind(owner).execute(pool).await.map_err(|e| e.to_string())?.rows_affected();
                if changed != 1 { return Err("maintenance claim lost; work cancelled".into()); }
            }
        }
    };
    if result.as_ref().is_ok_and(complete) {
        sqlx::query("UPDATE maintenance_claims SET completed_at=now(),updated_at=now() WHERE job_key=$1 AND owner=$2")
            .bind(key).bind(owner).execute(pool).await.map_err(|e| e.to_string())?;
    } else {
        sqlx::query("DELETE FROM maintenance_claims WHERE job_key=$1 AND owner=$2")
            .bind(key)
            .bind(owner)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    result.map(Some)
}

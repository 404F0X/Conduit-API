//! Persistent object ownership and deletion survive GC transactions and restarts.
use conduit_db::{
    PolicyContext, Principal, RequestContext, repo::data_storage_repo::DataStorageRepo,
};
use sqlx::{PgPool, Row};
use std::sync::Arc;

pub async fn enqueue_write(pool: &PgPool, storage: i64, key: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO artifact_deletion_queue(data_storage_id,object_key,delete_requested,next_attempt_at) VALUES($1,$2,FALSE,now()+interval '20 minutes') ON CONFLICT DO NOTHING")
        .bind(storage).bind(key).execute(pool).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn claim_key(storage: i64, key: &str) -> String {
    format!("artifact:{storage}:{key}")
}

pub async fn still_owned(pool: &PgPool, storage: i64, key: &str) -> Result<bool, String> {
    let parts: Vec<_> = key.split('/').collect();
    let project = parts
        .first()
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or("invalid artifact project")?;
    let request = parts
        .get(2)
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or("invalid artifact request")?;
    if parts.get(1) != Some(&"requests") {
        return Err("invalid artifact scope".into());
    }
    if parts.get(3) == Some(&"executions") {
        let execution = parts
            .get(4)
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or("invalid artifact execution")?;
        let column = parts
            .get(5)
            .and_then(|v| v.strip_suffix(".json"))
            .ok_or("invalid artifact resource")?;
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM request_executions WHERE id=$1 AND project_id=$2 AND request_id=$3 AND data_storage_id=$4 AND NOT($5=ANY(expired_artifacts)))")
            .bind(execution).bind(project).bind(request).bind(storage).bind(column).fetch_one(pool).await.map_err(|e| e.to_string())
    } else if parts.get(3) == Some(&"video") {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM requests WHERE id=$1 AND project_id=$2 AND NOT content_expired AND ((content_saved AND content_storage_id=$3 AND content_storage_key=$4) OR (NOT content_saved AND status IN ('processing','completed'))))")
            .bind(request).bind(project).bind(storage).bind(key).fetch_one(pool).await.map_err(|e| e.to_string())
    } else {
        let column = parts
            .get(3)
            .and_then(|v| v.strip_suffix(".json"))
            .ok_or("invalid artifact resource")?;
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM requests WHERE id=$1 AND project_id=$2 AND data_storage_id=$3 AND NOT($4=ANY(expired_artifacts)))")
            .bind(request).bind(project).bind(storage).bind(column).fetch_one(pool).await.map_err(|e| e.to_string())
    }
}

pub async fn retry(pool: &PgPool, repo: Arc<dyn DataStorageRepo>) -> Result<(), String> {
    let rows = sqlx::query("SELECT id,data_storage_id,object_key FROM artifact_deletion_queue WHERE next_attempt_at<=now() ORDER BY next_attempt_at,id LIMIT 100")
        .fetch_all(pool).await.map_err(|e| e.to_string())?;
    for row in rows {
        let id: i64 = row.get("id");
        let storage: i64 = row.get("data_storage_id");
        let key: String = row.get("object_key");
        let result = crate::maintenance_claim::run(pool, &claim_key(storage, &key), false, async {
            let requested: Option<bool> = sqlx::query_scalar(
                "SELECT delete_requested FROM artifact_deletion_queue WHERE id=$1",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
            let Some(requested) = requested else {
                return Ok(());
            };
            if requested || !still_owned(pool, storage, &key).await? {
                let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
                let target = repo
                    .find_data_storage_unchecked(&ctx, &storage.to_string())
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or("deletion storage missing; preserve queued work")?;
                crate::wiring_request_content::build_data_storage_service(&target)?
                    .delete_data(&key)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            // GC can upgrade a write intent while ownership is being checked.
            // Never erase that newly requested deletion with the old observation.
            sqlx::query("DELETE FROM artifact_deletion_queue WHERE id=$1 AND delete_requested=$2")
                .bind(id)
                .bind(requested)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await;
        if result.is_err() {
            sqlx::query("UPDATE artifact_deletion_queue SET attempts=LEAST(attempts+1,20),next_attempt_at=now()+make_interval(secs=>LEAST(3600,30*power(2,LEAST(attempts,7)))::int),last_error='artifact deletion failed; check storage availability' WHERE id=$1")
                .bind(id).execute(pool).await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

//! PostgreSQL system-operations and storage-policy garbage collection.
//!
//! Retention domains stay deliberately separate: request cleanup removes the
//! selected request rows and their executions, while usage logs are deleted
//! only by their own policy step.  Customer charge events, settlements,
//! credit ledgers, subscription buckets and every other billing table are
//! outside this executor and can never be selected by a GC plan.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use conduit_admin_graphql::scalars::TimeScalar;
use conduit_admin_graphql::system_operations_ext::{
    ClearCacheInput, ClearCachePayload, GcCleanupPreviewItem, GetCacheDiagnosticsInput,
    GetCacheDiagnosticsPayload, SystemOperationsError, SystemOperationsServices,
    TriggerGcCleanupInput, normalize_targets,
};
use conduit_cache::Cache;
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::{
    DEFAULT_GC_BATCH_SIZE, GcConfig, GcRunPlan, GcRunResource, SystemService,
    TriggerGcCleanupInput as DomainGcInput, build_gc_run_plan, build_manual_gc_run_plan,
    select_vacuum_sql,
};
use serde_json::json;
use sqlx::{PgPool, Row};

const CHANNEL_MODEL_CACHE_PREFIXES: [&str; 2] = ["channel:", "model:"];

pub struct PgSystemOperationsAdapter {
    pool: PgPool,
    cache: Arc<dyn Cache>,
    system: Arc<SystemService>,
    gc_config: GcConfig,
}

impl PgSystemOperationsAdapter {
    pub fn new(
        pool: PgPool,
        cache: Arc<dyn Cache>,
        system: Arc<SystemService>,
        gc_config: GcConfig,
    ) -> Self {
        Self {
            pool,
            cache,
            system,
            gc_config,
        }
    }

    async fn count_before(
        &self,
        table: &str,
        cutoff: chrono::DateTime<Utc>,
    ) -> Result<i32, SystemOperationsError> {
        // `table` is always one of the two literals below; it is never sourced
        // from GraphQL input.
        let sql = format!("SELECT COUNT(*)::BIGINT FROM {table} WHERE created_at < $1");
        let count: i64 = sqlx::query_scalar(&sql)
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(operation_error)?;
        i32::try_from(count).map_err(|_| {
            SystemOperationsError::Operation(format!("{table} count exceeds GraphQL Int range"))
        })
    }
}

#[async_trait]
impl SystemOperationsServices for PgSystemOperationsAdapter {
    async fn get_cache_diagnostics(
        &self,
        input: Option<GetCacheDiagnosticsInput>,
    ) -> Result<GetCacheDiagnosticsPayload, SystemOperationsError> {
        let targets = normalize_targets(input.and_then(|value| value.targets));
        let channels = sqlx::query(
            "SELECT id,name,\"type\",status,updated_at FROM channels \
             ORDER BY ordering_weight DESC,id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(operation_error)?
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<i64, _>("id"),
                "name": row.get::<String, _>("name"),
                "type": row.get::<String, _>("type"),
                "status": row.get::<String, _>("status"),
                "updatedAt": row.get::<chrono::DateTime<Utc>, _>("updated_at").to_rfc3339(),
            })
        })
        .collect::<Vec<_>>();
        let models = sqlx::query("SELECT id,model_id,status,updated_at FROM models ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(operation_error)?
            .into_iter()
            .map(|row| {
                json!({
                    "id": row.get::<i64, _>("id"),
                    "modelId": row.get::<String, _>("model_id"),
                    "status": row.get::<String, _>("status"),
                    "updatedAt": row.get::<chrono::DateTime<Utc>, _>("updated_at").to_rfc3339(),
                })
            })
            .collect::<Vec<_>>();
        let exported_at = Utc::now();
        let content = serde_json::to_string_pretty(&json!({
            "exportedAt": exported_at.to_rfc3339(),
            "targets": ["CHANNEL_CACHE"],
            "database": { "channels": channels, "models": models },
            "cache": {
                "backend": self.cache.get_type(),
                "targetPrefixes": CHANNEL_MODEL_CACHE_PREFIXES,
                "valuesIncluded": false,
            },
            "runtime": {
                "candidateSource": "postgresql_per_request",
                "candidateEntriesCached": false,
            },
        }))
        .map_err(operation_error)?;
        Ok(GetCacheDiagnosticsPayload {
            file_name: format!(
                "conduit-channel-model-cache-diagnostics-{}.json",
                exported_at.format("%Y%m%dT%H%M%SZ")
            ),
            content,
            targets,
        })
    }

    async fn clear_cache(
        &self,
        input: ClearCacheInput,
    ) -> Result<ClearCachePayload, SystemOperationsError> {
        let targets = normalize_targets(input.targets);
        for prefix in CHANNEL_MODEL_CACHE_PREFIXES {
            self.cache
                .invalidate_prefix(prefix)
                .await
                .map_err(operation_error)?;
        }
        Ok(ClearCachePayload {
            success: true,
            message: "cache cleared successfully".to_string(),
            targets,
        })
    }

    async fn preview_gc_cleanup(
        &self,
        input: TriggerGcCleanupInput,
    ) -> Result<Vec<GcCleanupPreviewItem>, SystemOperationsError> {
        let now = Utc::now();
        let mut items = Vec::new();
        for (resource, table, days) in [
            ("requests", "requests", input.requests_cleanup_days),
            ("usage_logs", "usage_logs", input.usage_logs_cleanup_days),
        ] {
            if days <= 0 {
                continue;
            }
            let cutoff = now - chrono::Duration::days(i64::from(days));
            items.push(GcCleanupPreviewItem {
                resource_type: resource.to_string(),
                estimated_count: self.count_before(table, cutoff).await?,
                cutoff_time: TimeScalar(cutoff),
                retention_days: days,
            });
        }
        Ok(items)
    }

    async fn trigger_gc_cleanup(
        &self,
        input: TriggerGcCleanupInput,
    ) -> Result<bool, SystemOperationsError> {
        let policy = self
            .system
            .storage_policy_or_default(&gc_request_context())
            .await;
        let input = DomainGcInput {
            requests_cleanup_days: i64::from(input.requests_cleanup_days),
            usage_logs_cleanup_days: i64::from(input.usage_logs_cleanup_days),
        };
        let plan =
            build_manual_gc_run_plan(&policy.cleanup_options, &input, &self.gc_config, Utc::now());
        let pool = self.pool.clone();
        let config = self.gc_config.clone();
        tokio::spawn(async move {
            execute_postgres_gc_plan(&pool, &config, &plan).await;
        });
        Ok(true)
    }
}

/// Result of one independently executed PostgreSQL cleanup step.
#[derive(Debug, Clone)]
pub(crate) struct PostgresGcStepReport {
    pub(crate) resource: GcRunResource,
    pub(crate) deleted_rows: Option<u64>,
    pub(crate) error: Option<String>,
}

/// Observable report used by tests and by the scheduled maintenance caller.
/// An error in one step is recorded here but never prevents later steps.
#[derive(Debug, Clone, Default)]
pub(crate) struct PostgresGcExecutionReport {
    pub(crate) steps: Vec<PostgresGcStepReport>,
    pub(crate) vacuum_sql: Option<&'static str>,
    pub(crate) vacuum_error: Option<String>,
}

/// High-level automatic-GC entry point for `maintenance::start_postgres`.
/// It re-reads the persisted storage policy for every run, so an admin policy
/// change takes effect without a process restart.
pub(crate) async fn run_postgres_storage_policy_gc(
    pool: &PgPool,
    system: &SystemService,
    run_config: &GcConfig,
) -> PostgresGcExecutionReport {
    let policy = system
        .storage_policy_or_default(&gc_request_context())
        .await;
    let plan = build_gc_run_plan(
        &policy.cleanup_options,
        false,
        &BTreeMap::new(),
        run_config,
        Utc::now(),
    );
    execute_postgres_gc_plan(pool, run_config, &plan).await
}

/// Execute a resolved plan with Go-compatible failure isolation.
///
/// Each resource is committed independently. PostgreSQL VACUUM is deliberately
/// outside every transaction: PostgreSQL rejects it inside `BEGIN`, and
/// `VACUUM FULL` additionally takes exclusive locks, so it only runs when the
/// operator explicitly enabled the corresponding GC setting.
pub(crate) async fn execute_postgres_gc_plan(
    pool: &PgPool,
    run_config: &GcConfig,
    plan: &GcRunPlan,
) -> PostgresGcExecutionReport {
    let mut report = PostgresGcExecutionReport::default();
    for unknown in &plan.unknown_resources {
        tracing::warn!(resource = %unknown, "postgres gc: unknown resource, skipping");
    }
    for step in &plan.steps {
        let result = match step.resource {
            GcRunResource::Requests => request_cascade_before(pool, step.cutoff_at).await,
            GcRunResource::Threads => {
                delete_created_at_in_batches(pool, "threads", step.cutoff_at).await
            }
            GcRunResource::Traces => {
                delete_created_at_in_batches(pool, "traces", step.cutoff_at).await
            }
            GcRunResource::UsageLogs => {
                delete_created_at_in_batches(pool, "usage_logs", step.cutoff_at).await
            }
            GcRunResource::ChannelProbes => {
                delete_channel_probes_in_batches(pool, step.cutoff_at).await
            }
            GcRunResource::RequestHeaders => {
                erase_request_content(pool, "request_headers", "NULL", step.cutoff_at).await
            }
            GcRunResource::RequestBodies => {
                erase_request_content(pool, "request_body", "'null'::jsonb", step.cutoff_at).await
            }
            GcRunResource::ResponseBodies => {
                erase_request_content(pool, "response_body", "NULL", step.cutoff_at).await
            }
            GcRunResource::ResponseChunks => {
                erase_request_content(pool, "response_chunks", "NULL", step.cutoff_at).await
            }
        };
        match result {
            Ok(rows) => {
                tracing::info!(
                    resource = ?step.resource,
                    retention_days = step.retention_days,
                    rows,
                    "postgres gc: cleanup step complete"
                );
                report.steps.push(PostgresGcStepReport {
                    resource: step.resource,
                    deleted_rows: Some(rows),
                    error: None,
                });
            }
            Err(error) => {
                tracing::error!(
                    resource = ?step.resource,
                    %error,
                    "postgres gc: cleanup step failed; continuing"
                );
                report.steps.push(PostgresGcStepReport {
                    resource: step.resource,
                    deleted_rows: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    if plan.run_vacuum {
        let sql = select_vacuum_sql(run_config.vacuum_full);
        report.vacuum_sql = Some(sql);
        if let Err(error) = execute_postgres_vacuum_sql(pool, sql).await {
            tracing::error!(%error, sql, "postgres gc: VACUUM failed");
            report.vacuum_error = Some(error.to_string());
        }
    }
    report
}

/// Erase content columns while preserving the request/execution audit facts.
/// The column names are selected only from fixed `GcRunResource` arms.
async fn erase_request_content(
    pool: &PgPool,
    column: &str,
    replacement: &str,
    cutoff: chrono::DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut affected = 0;
    for table in ["requests", "request_executions"] {
        affected += sqlx::query(&format!(
            "UPDATE {table} SET {column}={replacement}, updated_at=now() WHERE created_at < $1 AND {column} IS NOT NULL"
        ))
        .bind(cutoff)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    }
    tx.commit().await?;
    Ok(affected)
}

fn gc_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

async fn delete_created_at_in_batches(
    pool: &PgPool,
    table: &str,
    cutoff: chrono::DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let sql = format!(
        "WITH doomed AS ( \
           SELECT id FROM {table} WHERE created_at < $1 ORDER BY id LIMIT $2 \
         ) DELETE FROM {table} AS target USING doomed \
           WHERE target.id=doomed.id"
    );
    let mut total = 0_u64;
    for _ in 0..100_000 {
        let deleted = sqlx::query(&sql)
            .bind(cutoff)
            .bind(i64::from(DEFAULT_GC_BATCH_SIZE))
            .execute(pool)
            .await?
            .rows_affected();
        total += deleted;
        if deleted == 0 {
            break;
        }
    }
    Ok(total)
}

async fn delete_channel_probes_in_batches(
    pool: &PgPool,
    cutoff: chrono::DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let mut total = 0_u64;
    for _ in 0..100_000 {
        let deleted = sqlx::query(
            "WITH doomed AS ( \
               SELECT id FROM channel_probes WHERE \"timestamp\" < $1 ORDER BY id LIMIT $2 \
             ) DELETE FROM channel_probes AS target USING doomed \
               WHERE target.id=doomed.id",
        )
        .bind(cutoff.timestamp())
        .bind(i64::from(DEFAULT_GC_BATCH_SIZE))
        .execute(pool)
        .await?
        .rows_affected();
        total += deleted;
        if deleted == 0 {
            break;
        }
    }
    Ok(total)
}

/// Delete executions belonging to an expired request before deleting the
/// request itself. Executions are selected by the parent request boundary,
/// not by their own timestamp; this prevents an execution for a retained
/// request from being removed merely because an upstream attempt is old.
async fn request_cascade_before(
    pool: &PgPool,
    cutoff: chrono::DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let mut total = 0_u64;
    for _ in 0..100_000 {
        let mut transaction = pool.begin().await?;
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM requests WHERE created_at < $1 \
             ORDER BY id LIMIT $2 FOR UPDATE SKIP LOCKED",
        )
        .bind(cutoff)
        .bind(i64::from(DEFAULT_GC_BATCH_SIZE))
        .fetch_all(&mut *transaction)
        .await?;
        if ids.is_empty() {
            transaction.rollback().await?;
            break;
        }
        sqlx::query("DELETE FROM request_executions WHERE request_id=ANY($1)")
            .bind(&ids)
            .execute(&mut *transaction)
            .await?;
        let deleted = sqlx::query("DELETE FROM requests WHERE id=ANY($1) AND created_at < $2")
            .bind(&ids)
            .bind(cutoff)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        transaction.commit().await?;
        total += deleted;
        if deleted == 0 {
            break;
        }
    }
    Ok(total)
}

/// VACUUM cannot run inside a PostgreSQL transaction block. Acquiring a fresh
/// pooled connection and issuing exactly one statement preserves autocommit.
async fn execute_postgres_vacuum_sql(pool: &PgPool, sql: &str) -> Result<(), sqlx::Error> {
    let mut connection = pool.acquire().await?;
    sqlx::query(sql).execute(&mut *connection).await?;
    Ok(())
}

fn operation_error(error: impl std::fmt::Display) -> SystemOperationsError {
    SystemOperationsError::Operation(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;

    use chrono::Duration;
    use conduit_cache::MemoryCache;
    use conduit_services::{CleanupOption, StoragePolicy};
    use sqlx::postgres::PgPoolOptions;

    type TestError = Box<dyn std::error::Error + Send + Sync>;

    #[derive(Debug)]
    struct RetentionFixture {
        old_request_id: i64,
        recent_request_id: i64,
        old_request_execution_id: i64,
        retained_request_execution_id: i64,
        old_usage_id: i64,
        retained_usage_id: i64,
        old_thread_id: i64,
        recent_thread_id: i64,
        old_trace_id: i64,
        recent_trace_id: i64,
        old_probe_id: i64,
        recent_probe_id: i64,
        charge_event_id: i64,
        settlement_id: i64,
    }

    struct SeedContext<'a> {
        project_id: i64,
        api_key_id: i64,
        channel_id: i64,
        model_id: &'a str,
        suffix: &'a str,
    }

    async fn seed_retention_fixture(
        pool: &PgPool,
        context: SeedContext<'_>,
        now: chrono::DateTime<Utc>,
    ) -> Result<RetentionFixture, sqlx::Error> {
        let old_request_at = now - Duration::days(10);
        let recent_at = now - Duration::days(1);
        let old_usage_at = now - Duration::days(40);
        let old_request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (api_key_id,project_id,model_id,request_body,channel_id,status,created_at,updated_at) \
             VALUES($1,$2,$3,'{}'::jsonb,$4,'completed',$5,$5) RETURNING id",
        )
        .bind(context.api_key_id)
        .bind(context.project_id)
        .bind(context.model_id)
        .bind(context.channel_id)
        .bind(old_request_at)
        .fetch_one(pool)
        .await?;
        let recent_request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (api_key_id,project_id,model_id,request_body,channel_id,status,created_at,updated_at) \
             VALUES($1,$2,$3,'{}'::jsonb,$4,'completed',$5,$5) RETURNING id",
        )
        .bind(context.api_key_id)
        .bind(context.project_id)
        .bind(context.model_id)
        .bind(context.channel_id)
        .bind(recent_at)
        .fetch_one(pool)
        .await?;

        // A recent execution belonging to an expired parent must be removed
        // with that parent. Conversely, an old attempt belonging to a retained
        // request must survive: the cascade boundary is the request row.
        let old_request_execution_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO request_executions \
             (project_id,request_id,channel_id,model_id,request_body,status,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'{}'::jsonb,'completed',$5,$5) RETURNING id",
        )
        .bind(context.project_id)
        .bind(old_request_id)
        .bind(context.channel_id)
        .bind(context.model_id)
        .bind(recent_at)
        .fetch_one(pool)
        .await?;
        let retained_request_execution_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO request_executions \
             (project_id,request_id,channel_id,model_id,request_body,status,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'{}'::jsonb,'completed',$5,$5) RETURNING id",
        )
        .bind(context.project_id)
        .bind(recent_request_id)
        .bind(context.channel_id)
        .bind(context.model_id)
        .bind(old_request_at)
        .fetch_one(pool)
        .await?;

        // Usage retention is independent of request retention in both
        // directions.
        let retained_usage_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO usage_logs \
             (request_id,api_key_id,channel_id,project_id,model_id,total_tokens,total_cost,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,1,0.1,$6,$6) RETURNING id",
        )
        .bind(old_request_id)
        .bind(context.api_key_id)
        .bind(context.channel_id)
        .bind(context.project_id)
        .bind(context.model_id)
        .bind(recent_at)
        .fetch_one(pool)
        .await?;
        let old_usage_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO usage_logs \
             (request_id,api_key_id,channel_id,project_id,model_id,total_tokens,total_cost,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,1,0.2,$6,$6) RETURNING id",
        )
        .bind(recent_request_id)
        .bind(context.api_key_id)
        .bind(context.channel_id)
        .bind(context.project_id)
        .bind(context.model_id)
        .bind(old_usage_at)
        .fetch_one(pool)
        .await?;

        let old_thread_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO threads(project_id,thread_id,created_at,updated_at) \
             VALUES($1,$2,$3,$3) RETURNING id",
        )
        .bind(context.project_id)
        .bind(format!("old-thread-{}", context.suffix))
        .bind(old_request_at)
        .fetch_one(pool)
        .await?;
        let recent_thread_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO threads(project_id,thread_id,created_at,updated_at) \
             VALUES($1,$2,$3,$3) RETURNING id",
        )
        .bind(context.project_id)
        .bind(format!("recent-thread-{}", context.suffix))
        .bind(recent_at)
        .fetch_one(pool)
        .await?;
        let old_trace_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO traces(project_id,trace_id,created_at,updated_at) \
             VALUES($1,$2,$3,$3) RETURNING id",
        )
        .bind(context.project_id)
        .bind(format!("old-trace-{}", context.suffix))
        .bind(old_request_at)
        .fetch_one(pool)
        .await?;
        let recent_trace_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO traces(project_id,trace_id,created_at,updated_at) \
             VALUES($1,$2,$3,$3) RETURNING id",
        )
        .bind(context.project_id)
        .bind(format!("recent-trace-{}", context.suffix))
        .bind(recent_at)
        .fetch_one(pool)
        .await?;
        let old_probe_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_probes \
             (channel_id,total_request_count,success_request_count,timestamp) \
             VALUES($1,1,1,$2) RETURNING id",
        )
        .bind(context.channel_id)
        .bind(old_request_at.timestamp())
        .fetch_one(pool)
        .await?;
        let recent_probe_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_probes \
             (channel_id,total_request_count,success_request_count,timestamp) \
             VALUES($1,1,1,$2) RETURNING id",
        )
        .bind(context.channel_id)
        .bind(recent_at.timestamp())
        .fetch_one(pool)
        .await?;

        // Billing records point at a usage log that GC is allowed to remove.
        // They must remain immutable historical facts after that deletion.
        let charge_event_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO customer_charge_events \
             (usage_log_id,request_id,amount,currency,applied_rules_snapshot,usage_snapshot, \
              calculation_snapshot,status,created_at) \
             VALUES($1,$2,0.5,'STATION_CREDIT','[]'::jsonb,'{}'::jsonb,'{}'::jsonb,'settled',$3) \
             RETURNING id",
        )
        .bind(old_usage_id)
        .bind(recent_request_id)
        .bind(old_usage_at)
        .fetch_one(pool)
        .await?;
        let settlement_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO charge_settlements \
             (charge_event_id,amount_micros,subscription_amount_micros,credit_amount_micros, \
              status,detail_snapshot,created_at) \
             VALUES($1,500000,500000,0,'settled','{}'::jsonb,$2) RETURNING id",
        )
        .bind(charge_event_id)
        .bind(old_usage_at)
        .fetch_one(pool)
        .await?;

        Ok(RetentionFixture {
            old_request_id,
            recent_request_id,
            old_request_execution_id,
            retained_request_execution_id,
            old_usage_id,
            retained_usage_id,
            old_thread_id,
            recent_thread_id,
            old_trace_id,
            recent_trace_id,
            old_probe_id,
            recent_probe_id,
            charge_event_id,
            settlement_id,
        })
    }

    async fn row_exists(pool: &PgPool, table: &str, id: i64) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar::<_, bool>(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=$1)"))
            .bind(id)
            .fetch_one(pool)
            .await
    }

    fn policy(requests: bool, usage_logs: bool) -> StoragePolicy {
        StoragePolicy {
            cleanup_options: vec![
                CleanupOption {
                    resource_type: "requests".to_string(),
                    enabled: requests,
                    cleanup_days: 3,
                },
                CleanupOption {
                    resource_type: "usage_logs".to_string(),
                    enabled: usage_logs,
                    cleanup_days: 30,
                },
            ],
            ..StoragePolicy::default()
        }
    }

    #[tokio::test]
    async fn channel_cache_clear_preserves_unrelated_shared_cache_entries() -> Result<(), TestError>
    {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://conduit:password@127.0.0.1:1/conduit")?;
        let concrete_cache = Arc::new(MemoryCache::new(StdDuration::from_secs(300)));
        let cache: Arc<dyn Cache> = concrete_cache.clone();
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
            cache.clone(),
        ));
        let adapter = PgSystemOperationsAdapter::new(
            pool,
            cache,
            system,
            GcConfig {
                cron: String::new(),
                vacuum_enabled: false,
                vacuum_full: false,
            },
        );

        concrete_cache
            .set("channel:1:config", json!({"cached": true}), None)
            .await?;
        concrete_cache
            .set("model:gpt-4:config", json!({"cached": true}), None)
            .await?;
        concrete_cache
            .set("system:value:model_settings", json!({"cached": true}), None)
            .await?;
        concrete_cache
            .set("route_affinity:request-1", json!({"cached": true}), None)
            .await?;

        let cleared = adapter.clear_cache(ClearCacheInput::default()).await?;

        assert!(cleared.success);
        assert!(concrete_cache.get("channel:1:config").await?.is_none());
        assert!(concrete_cache.get("model:gpt-4:config").await?.is_none());
        assert!(
            concrete_cache
                .get("system:value:model_settings")
                .await?
                .is_some()
        );
        assert!(
            concrete_cache
                .get("route_affinity:request-1")
                .await?
                .is_some()
        );

        Ok(())
    }

    #[tokio::test]
    async fn live_postgres_system_operations_keep_retention_and_billing_boundaries()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let admin_pool = PgPool::connect(&dsn).await?;
        let schema = format!("system_operations_test_{}", uuid::Uuid::new_v4().simple());
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
            .connect(&dsn)
            .await?;

        let outcome: Result<(), TestError> = async {
            conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let project_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO projects(name,description,status) \
                 VALUES($1,'system operations test','active') RETURNING id",
            )
            .bind(format!("system-operations-project-{suffix}"))
            .fetch_one(&pool)
            .await?;
            let channel_name = format!("system-operations-channel-{suffix}");
            let channel_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
                 VALUES('openai',$1,'enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
            )
            .bind(&channel_name)
            .fetch_one(&pool)
            .await?;
            let api_key_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO api_keys(project_id,key,name,status) \
                 VALUES($1,$2,$3,'enabled') RETURNING id",
            )
            .bind(project_id)
            .bind(format!("conduit-system-operations-{suffix}"))
            .bind(format!("system-operations-key-{suffix}"))
            .fetch_one(&pool)
            .await?;
            let model_id = format!("system-operations-model-{suffix}");
            sqlx::query(
                "INSERT INTO models \
                 (developer,model_id,type,name,icon,\"group\",model_card,settings,status) \
                 VALUES('test',$1,'chat',$2,'','test','{}'::jsonb,'{}'::jsonb,'enabled')",
            )
            .bind(&model_id)
            .bind(format!("System Operations Model {suffix}"))
            .execute(&pool)
            .await?;

            let account_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO credit_accounts(user_id,currency,status,created_at,updated_at) \
                 VALUES(999999,'STATION_CREDIT','enabled',now() - interval '100 days',now()) RETURNING id",
            )
            .fetch_one(&pool)
            .await?;
            let ledger_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO credit_ledger_entries \
                 (account_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
                 VALUES($1,1000000,'grant',$2,'{}'::jsonb,now() - interval '100 days') RETURNING id",
            )
            .bind(account_id)
            .bind(format!("system-operations-ledger-{suffix}"))
            .fetch_one(&pool)
            .await?;

            let concrete_cache = Arc::new(MemoryCache::new(StdDuration::from_secs(300)));
            let cache: Arc<dyn Cache> = concrete_cache.clone();
            let system = Arc::new(SystemService::from_system_repo(
                Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
                cache.clone(),
            ));
            let gc_config = GcConfig {
                cron: String::new(),
                vacuum_enabled: false,
                vacuum_full: false,
            };
            let adapter = PgSystemOperationsAdapter::new(
                pool.clone(),
                cache,
                system.clone(),
                gc_config.clone(),
            );

            concrete_cache
                .set("channel:1:config", json!({"cached": true}), None)
                .await?;
            concrete_cache
                .set("model:gpt-4:config", json!({"cached": true}), None)
                .await?;
            concrete_cache
                .set("system:value:model_settings", json!({"cached": true}), None)
                .await?;
            let diagnostics = adapter.get_cache_diagnostics(None).await?;
            let diagnostics_json: serde_json::Value = serde_json::from_str(&diagnostics.content)?;
            assert!(diagnostics.file_name.ends_with(".json"));
            assert!(
                diagnostics_json["database"]["channels"]
                    .as_array()
                    .expect("channel diagnostics array")
                    .iter()
                    .any(|row| row["name"] == channel_name)
            );
            assert!(
                diagnostics_json["database"]["models"]
                    .as_array()
                    .expect("model diagnostics array")
                    .iter()
                    .any(|row| row["modelId"] == model_id)
            );
            let cleared = adapter.clear_cache(ClearCacheInput::default()).await?;
            assert!(cleared.success);
            assert!(concrete_cache.get("channel:1:config").await?.is_none());
            assert!(concrete_cache.get("model:gpt-4:config").await?.is_none());
            assert!(
                concrete_cache
                    .get("system:value:model_settings")
                    .await?
                    .is_some(),
                "clearing CHANNEL_CACHE must not flush unrelated shared-cache entries"
            );

            let now = Utc::now();
            let manual = seed_retention_fixture(
                &pool,
                SeedContext {
                    project_id,
                    api_key_id,
                    channel_id,
                    model_id: &model_id,
                    suffix: &format!("manual-{suffix}"),
                },
                now,
            )
            .await?;
            let preview = adapter
                .preview_gc_cleanup(TriggerGcCleanupInput {
                    requests_cleanup_days: 3,
                    usage_logs_cleanup_days: 30,
                })
                .await?;
            assert_eq!(preview.len(), 2);
            assert_eq!(preview[0].resource_type, "requests");
            assert_eq!(preview[0].estimated_count, 1);
            assert_eq!(preview[1].resource_type, "usage_logs");
            assert_eq!(preview[1].estimated_count, 1);

            assert!(
                adapter
                    .trigger_gc_cleanup(TriggerGcCleanupInput {
                        requests_cleanup_days: 3,
                        usage_logs_cleanup_days: 30,
                    })
                    .await?
            );
            // trigger_gc_cleanup is deliberately asynchronous. The probe step
            // is last, so its completion is a deterministic whole-plan fence.
            for _ in 0..100 {
                if !row_exists(&pool, "channel_probes", manual.old_probe_id).await? {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
            assert!(!row_exists(&pool, "requests", manual.old_request_id).await?);
            assert!(row_exists(&pool, "requests", manual.recent_request_id).await?);
            assert!(
                !row_exists(
                    &pool,
                    "request_executions",
                    manual.old_request_execution_id
                )
                .await?
            );
            assert!(
                row_exists(
                    &pool,
                    "request_executions",
                    manual.retained_request_execution_id
                )
                .await?
            );
            assert!(!row_exists(&pool, "usage_logs", manual.old_usage_id).await?);
            assert!(row_exists(&pool, "usage_logs", manual.retained_usage_id).await?);
            assert!(!row_exists(&pool, "threads", manual.old_thread_id).await?);
            assert!(row_exists(&pool, "threads", manual.recent_thread_id).await?);
            assert!(!row_exists(&pool, "traces", manual.old_trace_id).await?);
            assert!(row_exists(&pool, "traces", manual.recent_trace_id).await?);
            assert!(!row_exists(&pool, "channel_probes", manual.old_probe_id).await?);
            assert!(row_exists(&pool, "channel_probes", manual.recent_probe_id).await?);
            assert!(
                row_exists(&pool, "customer_charge_events", manual.charge_event_id).await?
            );
            assert!(row_exists(&pool, "charge_settlements", manual.settlement_id).await?);
            assert!(row_exists(&pool, "credit_ledger_entries", ledger_id).await?);

            // Automatic mode must obey the stored enable flags, unlike manual
            // mode which uses the caller's explicit day overrides.
            let automatic = seed_retention_fixture(
                &pool,
                SeedContext {
                    project_id,
                    api_key_id,
                    channel_id,
                    model_id: &model_id,
                    suffix: &format!("automatic-{suffix}"),
                },
                now,
            )
            .await?;
            system
                .set_storage_policy(&gc_request_context(), &policy(false, true))
                .await?;
            let usage_only =
                run_postgres_storage_policy_gc(&pool, &system, &gc_config).await;
            assert!(
                usage_only
                    .steps
                    .iter()
                    .all(|step| step.resource != GcRunResource::Requests)
            );
            assert!(row_exists(&pool, "requests", automatic.old_request_id).await?);
            assert!(!row_exists(&pool, "usage_logs", automatic.old_usage_id).await?);
            assert!(row_exists(&pool, "threads", automatic.old_thread_id).await?);
            assert!(!row_exists(&pool, "channel_probes", automatic.old_probe_id).await?);
            assert!(
                row_exists(
                    &pool,
                    "customer_charge_events",
                    automatic.charge_event_id
                )
                .await?
            );
            assert!(
                row_exists(&pool, "charge_settlements", automatic.settlement_id).await?
            );

            system
                .set_storage_policy(&gc_request_context(), &policy(true, false))
                .await?;
            run_postgres_storage_policy_gc(&pool, &system, &gc_config).await;
            assert!(!row_exists(&pool, "requests", automatic.old_request_id).await?);
            assert!(!row_exists(&pool, "threads", automatic.old_thread_id).await?);
            assert!(!row_exists(&pool, "traces", automatic.old_trace_id).await?);
            assert!(row_exists(&pool, "usage_logs", automatic.retained_usage_id).await?);

            // PostgreSQL rejects VACUUM inside BEGIN. This targeted statement
            // exercises the same fresh-connection/autocommit helper without
            // vacuuming unrelated schemas in the shared test database.
            execute_postgres_vacuum_sql(&pool, "VACUUM requests").await?;

            // A broken middle resource must be recorded while subsequent
            // traces, usage and probe steps still complete.
            let failure = seed_retention_fixture(
                &pool,
                SeedContext {
                    project_id,
                    api_key_id,
                    channel_id,
                    model_id: &model_id,
                    suffix: &format!("failure-{suffix}"),
                },
                now,
            )
            .await?;
            sqlx::query("DROP TABLE threads").execute(&pool).await?;
            let failure_plan = build_gc_run_plan(
                &policy(true, true).cleanup_options,
                false,
                &BTreeMap::new(),
                &gc_config,
                Utc::now(),
            );
            let failure_report =
                execute_postgres_gc_plan(&pool, &gc_config, &failure_plan).await;
            let thread_failure = failure_report
                .steps
                .iter()
                .find(|step| step.resource == GcRunResource::Threads)
                .expect("threads step report");
            assert!(thread_failure.deleted_rows.is_none());
            assert!(thread_failure.error.is_some());
            assert!(
                failure_report
                    .steps
                    .iter()
                    .find(|step| step.resource == GcRunResource::Traces)
                    .and_then(|step| step.deleted_rows)
                    .is_some()
            );
            assert!(!row_exists(&pool, "requests", failure.old_request_id).await?);
            assert!(!row_exists(&pool, "traces", failure.old_trace_id).await?);
            assert!(!row_exists(&pool, "usage_logs", failure.old_usage_id).await?);
            assert!(!row_exists(&pool, "channel_probes", failure.old_probe_id).await?);
            assert!(
                row_exists(&pool, "customer_charge_events", failure.charge_event_id).await?
            );
            assert!(row_exists(&pool, "charge_settlements", failure.settlement_id).await?);
            assert!(row_exists(&pool, "credit_ledger_entries", ledger_id).await?);
            assert!(failure_report.vacuum_sql.is_none());
            assert!(failure_report.vacuum_error.is_none());
            Ok(())
        }
        .await;

        pool.close().await;
        let cleanup = sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
            .execute(&admin_pool)
            .await;
        admin_pool.close().await;
        outcome?;
        cleanup?;
        Ok(())
    }
}

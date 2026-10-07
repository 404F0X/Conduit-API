use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use conduit_cache::NoopCache;
use conduit_config::model::GcConfig;
use conduit_scheduler::{JobSpec, Scheduler, SchedulerWorkers, TaskSupervisor};
use conduit_services::{GcConfig as GcRunConfig, SystemService as DomainSystemService};
use sqlx::PgPool;

pub struct MaintenanceRuntime {
    scheduler: Scheduler,
    workers: SchedulerWorkers,
    tasks: Arc<TaskSupervisor>,
}

impl MaintenanceRuntime {
    pub async fn shutdown(self) {
        self.scheduler.shutdown();
        self.tasks.cancel();
        self.workers.shutdown().await;
        self.tasks.shutdown(Duration::from_secs(10)).await;
    }
}

pub async fn start_postgres(
    pool: PgPool,
    config: &GcConfig,
    quota_config: &conduit_config::model::ProviderQuotaConfig,
    live_registry: Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
    tasks: Arc<TaskSupervisor>,
) -> Result<MaintenanceRuntime, String> {
    let mut scheduler = Scheduler::new();
    let maintenance_system = Arc::new(DomainSystemService::from_system_repo(
        Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
        Arc::new(NoopCache::new()),
    ));
    let billing_pool = pool.clone();
    scheduler
        .register_job(JobSpec::new(
            "billing.subscription_lifecycle",
            Duration::from_secs(60),
            move |_| {
                let adapter =
                    crate::wiring_postgres_billing::PgBillingAdapter::new(billing_pool.clone());
                async move {
                    match adapter.process_due_subscriptions().await {
                        Ok(processed) if processed > 0 => {
                            tracing::info!(processed, "processed due subscriptions");
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::error!(%error, "subscription lifecycle maintenance failed");
                        }
                    }
                }
            },
        ))
        .map_err(|error| error.to_string())?;
    if config.enabled && config.stale_processing_enabled {
        let stale_pool = pool.clone();
        let stale_after = config.stale_processing_interval;
        scheduler
            .register_job(JobSpec::new(
                "gc.stale_processing",
                nonzero_interval(config.stale_processing_interval),
                move |_| {
                    let pool = stale_pool.clone();
                    async move {
                        if let Err(error) =
                            mark_stale_processing_postgres(&pool, stale_after).await
                        {
                            tracing::error!(%error, "PostgreSQL stale-processing maintenance failed");
                        }
                    }
                },
            ))
            .map_err(|error| error.to_string())?;
    }
    if config.enabled {
        let gc_pool = pool.clone();
        let gc_system = maintenance_system.clone();
        let gc_config = GcRunConfig {
            cron: String::new(),
            vacuum_enabled: config.vacuum_enabled,
            vacuum_full: config.vacuum_full,
        };
        scheduler
            .register_job(JobSpec::new(
                "gc.storage_policy_cleanup",
                Duration::from_secs(24 * 60 * 60),
                move |_| {
                    let pool = gc_pool.clone();
                    let system = gc_system.clone();
                    let config = gc_config.clone();
                    async move {
                        let claimed = crate::maintenance_claim::run(&pool, "storage-policy-gc", false, async {
                        let report = crate::wiring_postgres_system_operations::run_postgres_storage_policy_gc(
                            &pool, &system, &config,
                        )
                        .await;
                        let deleted_rows = report
                            .steps
                            .iter()
                            .filter_map(|step| step.deleted_rows)
                            .sum::<u64>();
                        let failed_resources = report
                            .steps
                            .iter()
                            .filter_map(|step| {
                                step.error
                                    .as_ref()
                                    .map(|_| format!("{:?}", step.resource))
                            })
                            .collect::<Vec<_>>();
                        tracing::info!(
                            steps = report.steps.len(),
                            deleted_rows,
                            failed_steps = failed_resources.len(),
                            failed_resources = ?failed_resources,
                            "PostgreSQL storage-policy GC run complete"
                        );
                        Ok(())
                        }).await;
                        if let Err(error) = claimed { tracing::error!(%error, "storage-policy GC claim failed"); }
                    }
                },
            ))
            .map_err(|error| error.to_string())?;
    }
    let model_sync = Arc::new(
        crate::wiring_postgres_channel_model_sync::PgChannelModelSyncAdapter::new(pool.clone())
            .with_dynamic_settings(maintenance_system.clone()),
    );
    periodic(&tasks, Duration::from_secs(3600), move || {
        let adapter = model_sync.clone();
        async move { adapter.run().await }
    });
    let probe = Arc::new(
        crate::wiring_postgres_channel_probe::PgChannelProbeAdapter::new(pool.clone())
            .with_dynamic_settings(maintenance_system.clone()),
    );
    let probe_pool = pool.clone();
    periodic(&tasks, Duration::from_secs(60), move || {
        let adapter = probe.clone();
        let pool = probe_pool.clone();
        async move {
            if let Some((aligned, minutes)) = adapter.current_probe_plan(Utc::now(), 1).await {
                let key = format!("channel-probe:{}", aligned.timestamp());
                crate::maintenance_claim::run(&pool, &key, true, async {
                    adapter
                        .compute_and_store(aligned, minutes)
                        .await
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
                .await?;
            }
            Ok(())
        }
    });
    let sweep = Arc::new(
        crate::wiring_postgres_channel_probe::PgLiveStreamSweepAdapter::new(live_registry),
    );
    periodic(&tasks, Duration::from_secs(300), move || {
        let adapter = sweep.clone();
        async move { conduit_scheduler::LiveStreamSweepExecutor::sweep(adapter.as_ref(), 5).map(|_| ()) }
    });
    let video = Arc::new(
        crate::wiring_postgres_video_storage::PgVideoStorageAdapter::new(
            pool.clone(),
            maintenance_system.clone(),
            Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone())),
        ),
    );
    periodic(&tasks, Duration::from_secs(60), move || {
        let adapter = video.clone();
        async move { adapter.run().await }
    });
    let backup = Arc::new(
        crate::wiring_postgres_backup::PgBackupExtAdapter::new(
            pool.clone(),
            maintenance_system.clone(),
            Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone())),
        )
        .with_task_supervisor(tasks.clone()),
    );
    periodic(&tasks, Duration::from_secs(3600), move || {
        let adapter = backup.clone();
        async move { adapter.run_scheduled().await }
    });
    if quota_config.enabled {
        let quota = Arc::new(
            crate::wiring_postgres_provider_quota::PgProviderQuotaAdapter::new(
                pool.clone(),
                maintenance_system,
            )
            .with_interval(quota_config.check_interval),
        );
        periodic(
            &tasks,
            nonzero_interval(quota_config.check_interval),
            move || {
                let adapter = quota.clone();
                async move { adapter.check(false).await }
            },
        );
    }
    let cleanup_pool = pool.clone();
    let cleanup_repo = Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone()));
    periodic(&tasks, Duration::from_secs(30), move || {
        let pool = cleanup_pool.clone();
        let repo = cleanup_repo.clone();
        async move { crate::artifact_cleanup::retry(&pool, repo).await }
    });
    let workers = scheduler.start();
    Ok(MaintenanceRuntime {
        scheduler,
        workers,
        tasks,
    })
}

fn periodic<F, Fut>(tasks: &TaskSupervisor, interval: Duration, work: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send,
{
    tasks.spawn(async move {
        let mut ticks = tokio::time::interval(interval);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticks.tick().await;
            if let Err(error) = work().await {
                tracing::error!(%error, "maintenance work failed; next interval will retry");
            }
        }
    });
}

#[cfg(test)]
mod postgres_tests {
    use super::*;

    #[tokio::test]
    async fn postgres_stale_processing_marks_request_and_execution_failed_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let marker = format!("stale-pg-{}", uuid::Uuid::new_v4().simple());
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (project_id,model_id,request_body,status,updated_at) \
             VALUES(1,$1,'{}'::jsonb,'processing',now()-interval '1 day') RETURNING id",
        )
        .bind(&marker)
        .fetch_one(&pool)
        .await?;
        let execution_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO request_executions \
             (project_id,request_id,model_id,request_body,status,updated_at) \
             VALUES(1,$1,$2,'{}'::jsonb,'processing',now()-interval '1 day') RETURNING id",
        )
        .bind(request_id)
        .bind(&marker)
        .fetch_one(&pool)
        .await?;

        assert_eq!(
            mark_stale_processing_postgres(&pool, Duration::from_secs(60)).await?,
            2
        );
        let request_status =
            sqlx::query_scalar::<_, String>("SELECT status FROM requests WHERE id=$1")
                .bind(request_id)
                .fetch_one(&pool)
                .await?;
        let (execution_status, execution_error) = sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT status,error_message FROM request_executions WHERE id=$1",
        )
        .bind(execution_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(request_status, "failed");
        assert_eq!(execution_status, "failed");
        assert_eq!(execution_error.as_deref(), Some("stale processing request"));

        sqlx::query("DELETE FROM request_executions WHERE id=$1")
            .bind(execution_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM requests WHERE id=$1")
            .bind(request_id)
            .execute(&pool)
            .await?;
        Ok(())
    }
}

fn nonzero_interval(interval: Duration) -> Duration {
    if interval.is_zero() {
        Duration::from_secs(60)
    } else {
        interval
    }
}

pub(crate) async fn mark_stale_processing_postgres(
    pool: &PgPool,
    stale_after: Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now()
        - chrono::Duration::from_std(stale_after).unwrap_or_else(|_| chrono::Duration::minutes(1));
    let mut transaction = pool.begin().await?;
    let requests = sqlx::query(
        "UPDATE requests SET status='failed',updated_at=now() \
         WHERE status='processing' AND updated_at<$1 AND COALESCE(activity_until,updated_at)<now()",
    )
    .bind(cutoff)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let executions = sqlx::query(
        "UPDATE request_executions SET status='failed', \
         error_message=COALESCE(error_message,'stale processing request'),updated_at=now() \
         WHERE status='processing' AND updated_at<$1 AND COALESCE(activity_until,updated_at)<now()",
    )
    .bind(cutoff)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok(requests + executions)
}

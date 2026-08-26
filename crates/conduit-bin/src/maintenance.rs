use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use conduit_cache::NoopCache;
use conduit_config::model::GcConfig;
use conduit_scheduler::AutoSyncFrequency;
use conduit_scheduler::{
    AutoBackupExecutor, AutoBackupWorker, BackupFrequency, ChannelModelSyncExecutor,
    ChannelModelSyncWorker, ChannelProbeExecutor, ChannelProbeWorker, LiveStreamSweepExecutor,
    LiveStreamSweepInterval, LiveStreamSweeperWorker, ProbeFrequency, VideoStorageExecutor,
    VideoStorageScanInterval, VideoStorageWorker,
};
use conduit_scheduler::{
    CancellationToken, JobSpec, ProviderQuotaCheckExecutor, ProviderQuotaCheckInterval,
    ProviderQuotaCheckWorker, Scheduler, SchedulerWorkers, run_worker_loop,
};
use conduit_services::{GcConfig as GcRunConfig, SystemService as DomainSystemService};
use sqlx::PgPool;
use tokio::task::JoinHandle;

pub struct MaintenanceRuntime {
    scheduler: Scheduler,
    workers: SchedulerWorkers,
    /// P-02: shutdown handle + join for the business workers spawned via
    /// `run_worker_loop` (currently the channel-probe worker; more follow as
    /// their executors are ported). Empty when no worker was spawned.
    worker_shutdown: CancellationToken,
    worker_joins: Vec<JoinHandle<()>>,
}

impl MaintenanceRuntime {
    pub async fn shutdown(mut self) {
        // Stop the GC jobs first, then signal + await the business workers.
        self.scheduler.shutdown();
        self.workers.shutdown().await;
        self.worker_shutdown.cancel();
        while let Some(join) = self.worker_joins.pop() {
            let _ = join.await;
        }
    }
}

pub async fn start_postgres(
    pool: PgPool,
    config: &GcConfig,
    quota_config: &conduit_config::model::ProviderQuotaConfig,
    live_registry: Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
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
                    }
                },
            ))
            .map_err(|error| error.to_string())?;
    }
    let worker_shutdown = CancellationToken::new();
    let model_sync_executor: Arc<dyn ChannelModelSyncExecutor> = Arc::new(
        crate::wiring_postgres_channel_model_sync::PgChannelModelSyncAdapter::new(pool.clone())
            .with_dynamic_settings(maintenance_system.clone()),
    );
    let model_sync = ChannelModelSyncWorker::new(
        ChannelModelSyncWorker::DEFAULT_NAME,
        // Poll hourly and apply the current 1h/6h/1d setting in the executor.
        AutoSyncFrequency::OneHour,
        true,
    )
    .with_executor(model_sync_executor);
    let model_sync_join = tokio::spawn(run_worker_loop(
        model_sync,
        worker_shutdown.clone(),
        Utc::now,
    ));
    let probe_executor: Arc<dyn ChannelProbeExecutor> = Arc::new(
        crate::wiring_postgres_channel_probe::PgChannelProbeAdapter::new(pool.clone())
            .with_dynamic_settings(maintenance_system.clone()),
    );
    let probe = ChannelProbeWorker::new(
        ChannelProbeWorker::DEFAULT_NAME,
        // Poll each minute so enable/frequency updates apply without restart.
        ProbeFrequency::OneMinute,
        true,
    )
    .with_executor(probe_executor);
    let probe_join = tokio::spawn(run_worker_loop(probe, worker_shutdown.clone(), Utc::now));

    let sweep_executor: Arc<dyn LiveStreamSweepExecutor> = Arc::new(
        crate::wiring_postgres_channel_probe::PgLiveStreamSweepAdapter::new(live_registry),
    );
    let sweeper = LiveStreamSweeperWorker::new(
        LiveStreamSweeperWorker::DEFAULT_NAME,
        LiveStreamSweepInterval::DEFAULT,
        true,
    )
    .with_executor(sweep_executor);
    let sweeper_join = tokio::spawn(run_worker_loop(sweeper, worker_shutdown.clone(), Utc::now));

    let video_executor: Arc<dyn VideoStorageExecutor> = Arc::new(
        crate::wiring_postgres_video_storage::PgVideoStorageAdapter::new(
            pool.clone(),
            maintenance_system.clone(),
            Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone())),
        ),
    );
    let video = VideoStorageWorker::new(
        VideoStorageWorker::DEFAULT_NAME,
        // Poll settings once per minute. The executor applies the current
        // persisted scan interval, so updates do not require a process restart.
        VideoStorageScanInterval::from_minutes(1),
        true,
    )
    .with_executor(video_executor);
    let video_join = tokio::spawn(run_worker_loop(video, worker_shutdown.clone(), Utc::now));

    let backup_adapter = Arc::new(crate::wiring_postgres_backup::PgBackupExtAdapter::new(
        pool.clone(),
        maintenance_system.clone(),
        Arc::new(conduit_db::PgDataStorageRepo::new(pool.clone())),
    ));
    let backup_executor: Arc<dyn AutoBackupExecutor> = backup_adapter;
    let backup =
        AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true)
            .with_poll_interval(Duration::from_secs(60 * 60))
            .with_executor(backup_executor);
    let backup_join = tokio::spawn(run_worker_loop(backup, worker_shutdown.clone(), Utc::now));

    let quota_executor: Arc<dyn ProviderQuotaCheckExecutor> = Arc::new(
        crate::wiring_postgres_provider_quota::PgProviderQuotaAdapter::new(
            pool,
            maintenance_system,
        )
        .with_interval(quota_config.check_interval),
    );
    let quota_minutes = i64::try_from(quota_config.check_interval.as_secs() / 60)
        .unwrap_or(i64::MAX)
        .max(1);
    let quota = ProviderQuotaCheckWorker::new(
        ProviderQuotaCheckWorker::DEFAULT_NAME,
        ProviderQuotaCheckInterval::round_from_minutes(quota_minutes),
        quota_config.enabled,
    )
    .with_executor(quota_executor);
    let quota_join = tokio::spawn(run_worker_loop(quota, worker_shutdown.clone(), Utc::now));
    let workers = scheduler.start();
    Ok(MaintenanceRuntime {
        scheduler,
        workers,
        worker_shutdown,
        worker_joins: vec![
            model_sync_join,
            probe_join,
            sweeper_join,
            video_join,
            backup_join,
            quota_join,
        ],
    })
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

async fn mark_stale_processing_postgres(
    pool: &PgPool,
    stale_after: Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = Utc::now()
        - chrono::Duration::from_std(stale_after).unwrap_or_else(|_| chrono::Duration::minutes(1));
    let mut transaction = pool.begin().await?;
    let requests = sqlx::query(
        "UPDATE requests SET status='failed',updated_at=now() \
         WHERE status='processing' AND updated_at<$1",
    )
    .bind(cutoff)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    let executions = sqlx::query(
        "UPDATE request_executions SET status='failed', \
         error_message=COALESCE(error_message,'stale processing request'),updated_at=now() \
         WHERE status='processing' AND updated_at<$1",
    )
    .bind(cutoff)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok(requests + executions)
}

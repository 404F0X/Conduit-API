#![forbid(unsafe_code)]

pub mod jobs;
pub mod runtime;
pub mod worker;
pub mod worker_logic;

pub use jobs::{
    CancellationToken, GcWorkerSwitches, JOB_BACKUP, JOB_GC_REQUESTS_CLEANUP,
    JOB_GC_STALE_PROCESSING, JOB_GC_USAGE_LOGS_CLEANUP, JOB_PROVIDER_QUOTA, JOB_VIDEO_STORAGE,
    JobContext, JobHandle, JobRegistry, JobRegistryError, JobRun, JobRunError, JobSkipReason,
    JobSpec, Scheduler, SchedulerRegisterError, SchedulerWorkerSwitches, SchedulerWorkers,
    WorkerJobKind, WorkerJobSwitch, WorkerPlan, WorkerPlanEntry,
};
pub use worker::{
    AutoBackupExecutor, AutoBackupWorker, ChannelModelSyncExecutor, ChannelModelSyncWorker,
    ChannelProbeExecutor, ChannelProbeWorker, DataStorageSyncWorker, LiveStreamSweepExecutor,
    LiveStreamSweeperWorker, PromptCacheWorker, ProviderQuotaCheckExecutor,
    ProviderQuotaCheckWorker, VideoStorageExecutor, VideoStorageWorker, Worker, WorkerTickContext,
    WorkerTickOutcome, register_auto_backup_worker, register_channel_model_sync_worker,
    register_channel_probe_worker, register_data_storage_sync_worker,
    register_live_stream_sweeper_worker, register_prompt_cache_worker,
    register_provider_quota_check_worker, register_video_storage_worker, run_worker_loop,
};
pub use worker_logic::{
    AlignInterval, AutoSyncFrequency, BackupFrequency, DataStorageReloadInterval, JobState,
    LiveStreamSweepInterval, ProbeFrequency, PromptCacheReloadInterval, ProviderQuotaCheckInterval,
    ReentryDecision, RunningSnapshot, ShutdownPlan, VideoStorageScanInterval, WorkerSwitches,
    align_to_interval, decide_reentry, decide_run, decide_shutdown_transition, is_enabled,
    should_run_aligned, should_run_backup,
};

pub const CRATE_NAME: &str = "conduit-scheduler";

pub use runtime::{WorkerDefaults, WorkerRuntime, spawn_all_workers};

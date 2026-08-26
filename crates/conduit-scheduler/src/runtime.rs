//! fx wiring — register + spawn all P13-004 workers in one place.
//!
//! Mirrors the Go `fx.Invoke(...)` OnStart hooks that attach
//! `svc.RegisterScheduledTasks(ctx, s)` for each scheduled service:
//!
//! * `biz/fx_module.go:62-80`  — `ChannelService.RegisterScheduledTasks`
//!   (channel-model-sync) + `initChannelPerformances` side goroutine.
//! * `biz/fx_module.go:81-87`  — `DataStorageService.RegisterScheduledTasks`
//!   (datastorage-fs-reload).
//! * `biz/fx_module.go:88-94`  — `ChannelProbeService.RegisterScheduledTasks`
//!   (channel-probe).
//! * `biz/fx_module.go:95-101` — `PromptService.RegisterScheduledTasks`
//!   (prompt-cache).
//! * `biz/fx_module.go:45-61`  — `LiveStreamRegistry.StartSweeper` (the
//!   sweeper goroutine; NOT a `scheduler.Register` call but a directly spawned
//!   ticker loop, modeled here as the [`LiveStreamSweeperWorker`]).
//! * `biz/fx_module.go:110-116` — `ProviderQuotaService.RegisterScheduledTasks`
//!   (provider-quota-check).
//! * `backup/fx_module.go:13-19` — `BackupService.RegisterScheduledTasks`
//!   (backup).
//! * `video_storage/fx_module.go:13-19` — `video_storage.Worker.RegisterScheduledTasks`
//!   (video-storage).
//!
//! Go fires all of these inside the uber/fx `OnStart` phase against the same
//! `*scheduler.Scheduler` instance. The Rust projection below mirrors that by:
//!
//! 1. Building one [`Scheduler`] (the caller owns it; this module does NOT
//!    construct it, so callers can pre-register other jobs first).
//! 2. Calling each worker's `register_*` helper against that scheduler — the
//!    order follows Go's biz `fx_module.go` so registry listing matches.
//! 3. Spawning a [`run_worker_loop`] per worker whose `enabled()` is true —
//!    disabled workers are registered (for listing parity with Go, which always
//!    registers then short-circuits inside the callback) but NOT spawned.
//!
//! This module remains a generic registration/runtime helper. Production IO
//! executors are injected by `conduit-bin::maintenance`; workers without a
//! process-local cache are intentional no-ops in the Rust architecture.

use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;

use crate::jobs::{CancellationToken, Scheduler, SchedulerRegisterError};
use crate::worker::{
    AutoBackupWorker, ChannelModelSyncWorker, ChannelProbeWorker, DataStorageSyncWorker,
    LiveStreamSweeperWorker, PromptCacheWorker, ProviderQuotaCheckWorker, VideoStorageWorker,
    Worker, run_worker_loop,
};
use crate::worker_logic::{
    AutoSyncFrequency, BackupFrequency, DataStorageReloadInterval, LiveStreamSweepInterval,
    ProbeFrequency, PromptCacheReloadInterval, ProviderQuotaCheckInterval,
    VideoStorageScanInterval,
};

/// Default worker specifications for the 8 P13-004 workers.
///
/// Mirrors Go's per-service zero-value construction + the canonical default
/// frequencies observed in the Go source:
///
/// | Worker | Go default | Source |
/// |--------|------------|--------|
/// | channel-probe | `ProbeFrequency::FiveMinutes` | `setting.Probe.Frequency` default |
/// | channel-model-sync | `AutoSyncFrequency::OneHour` | `setting.AutoSync.Frequency` default |
/// | datastorage-fs-reload | `DataStorageReloadInterval::DEFAULT` (1m) | `data_storage.go:81` |
/// | provider-quota-check | `ProviderQuotaCheckInterval::default()` (5m) | `provider_quota.go:388-394` |
/// | live-stream-sweeper | `LiveStreamSweepInterval::DEFAULT` (5m) | `stream_preview.go:112` |
/// | prompt-cache | `PromptCacheReloadInterval::DEFAULT` (1m) | `prompt.go:73` |
/// | video-storage | `VideoStorageScanInterval::DEFAULT` (1m) | `worker.go:61-64` |
/// | backup | `BackupFrequency::Daily` | `backup/autobackup.go:74-85` |
///
/// All workers default to `enabled = true` except `backup`, which Go defaults
/// to `settings.Enabled = false` (`SchedulerWorkerSwitches::default().backup.enabled = false`
/// in `jobs.rs`).
#[derive(Debug, Clone)]
pub struct WorkerDefaults {
    /// Probe frequency for [`ChannelProbeWorker`].
    pub probe_frequency: ProbeFrequency,
    /// Auto-sync frequency for [`ChannelModelSyncWorker`].
    pub auto_sync_frequency: AutoSyncFrequency,
    /// Reload interval for [`DataStorageSyncWorker`].
    pub data_storage_interval: DataStorageReloadInterval,
    /// Check interval for [`ProviderQuotaCheckWorker`].
    pub provider_quota_interval: ProviderQuotaCheckInterval,
    /// Sweep interval for [`LiveStreamSweeperWorker`].
    pub live_stream_sweep_interval: LiveStreamSweepInterval,
    /// Reload interval for [`PromptCacheWorker`].
    pub prompt_cache_interval: PromptCacheReloadInterval,
    /// Scan interval for [`VideoStorageWorker`].
    pub video_storage_interval: VideoStorageScanInterval,
    /// Backup frequency for [`AutoBackupWorker`].
    pub backup_frequency: BackupFrequency,
    /// Per-worker enable flags. Order matches Go's `SchedulerWorkerSwitches`
    /// defaults — most workers default to enabled; backup defaults to disabled.
    pub enabled_probe: bool,
    pub enabled_model_sync: bool,
    pub enabled_data_storage: bool,
    pub enabled_provider_quota: bool,
    pub enabled_live_stream_sweeper: bool,
    pub enabled_prompt_cache: bool,
    pub enabled_video_storage: bool,
    pub enabled_backup: bool,
}

impl Default for WorkerDefaults {
    fn default() -> Self {
        Self {
            probe_frequency: ProbeFrequency::FiveMinutes,
            auto_sync_frequency: AutoSyncFrequency::OneHour,
            data_storage_interval: DataStorageReloadInterval::DEFAULT,
            provider_quota_interval: ProviderQuotaCheckInterval::default(),
            live_stream_sweep_interval: LiveStreamSweepInterval::DEFAULT,
            prompt_cache_interval: PromptCacheReloadInterval::DEFAULT,
            video_storage_interval: VideoStorageScanInterval::DEFAULT,
            backup_frequency: BackupFrequency::Daily,
            // Most workers default to enabled, matching Go's per-callback lack
            // of an explicit `if !enabled` early return (the cadence IS the
            // gate). Backup defaults to disabled, matching Go
            // `SchedulerWorkerSwitches::default().backup.enabled = false`.
            enabled_probe: true,
            enabled_model_sync: true,
            enabled_data_storage: true,
            enabled_provider_quota: true,
            enabled_live_stream_sweeper: true,
            enabled_prompt_cache: true,
            enabled_video_storage: true,
            enabled_backup: false,
        }
    }
}

/// Owning handle for the spawn-side of the P13-004 wiring.
///
/// Constructed by [`spawn_all_workers`]. Holds the join handle for each
/// spawned loop (one per enabled worker). Dropping this struct does NOT cancel
/// the loops — call [`WorkerRuntime::shutdown`] (or fire the [`CancellationToken`]
/// handed to [`spawn_all_workers`]) to terminate them, mirroring Go's fx
/// `OnStop` hooks.
///
/// The registered worker instances are NOT retained here: `run_worker_loop`
/// takes ownership of the worker, so the only handle we keep is the tokio task
/// join. The scheduler's registry (queryable via `Scheduler::registry()`) is
/// the source of truth for "what got registered" — mirroring Go, where the
/// `*scheduler.Scheduler` is the registry and the services don't retain
/// per-worker handles either.
pub struct WorkerRuntime {
    /// Join handles for each spawned loop (only enabled workers have one).
    /// Mirrors Go's implicitly-cancelable goroutines — the caller awaits these
    /// on shutdown via [`WorkerRuntime::shutdown`].
    pub joins: Vec<JoinHandle<()>>,
}

impl WorkerRuntime {
    /// Number of spawned loops (i.e. enabled workers at spawn time).
    pub fn spawned_count(&self) -> usize {
        self.joins.len()
    }

    /// Names of all 8 registered workers, in Go's biz `fx_module.go` order.
    ///
    /// Used by tests to assert the registry contains exactly these 8 distinct
    /// names. The order matches Go's OnStart invocation order. These are the
    /// canonical Go `TaskSpec.Name` strings (except `live-stream-sweeper`,
    /// which Go does NOT register with the scheduler — it's the Rust-side
    /// identifier for listing parity).
    pub fn registered_names() -> [&'static str; 8] {
        [
            ChannelProbeWorker::DEFAULT_NAME,
            ChannelModelSyncWorker::DEFAULT_NAME,
            DataStorageSyncWorker::DEFAULT_NAME,
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            LiveStreamSweeperWorker::DEFAULT_NAME,
            PromptCacheWorker::DEFAULT_NAME,
            VideoStorageWorker::DEFAULT_NAME,
            AutoBackupWorker::DEFAULT_NAME,
        ]
    }

    /// Signal all loops to stop via `shutdown` and await their joins.
    ///
    /// Mirrors Go's fx `OnStop` hooks (`biz/fx_module.go:54-60` cancels the
    /// sweeper's bgCtx; the cron scheduler's `Shutdown` cancels every
    /// registered task). Safe to call multiple times — the token is idempotent.
    pub async fn shutdown(mut self, shutdown: &CancellationToken) {
        shutdown.cancel();
        while let Some(join) = self.joins.pop() {
            let _ = join.await;
        }
    }
}

/// Register + spawn all 8 P13-004 workers into the given [`Scheduler`].
///
/// Mirrors the collective Go `fx.Invoke(...) OnStart` registration in
/// `biz/fx_module.go` + `backup/fx_module.go` + `video_storage/fx_module.go`.
/// The caller owns the [`Scheduler`] so it can pre-register other jobs (e.g.
/// the GC cleanup jobs, whose logic lives in `conduit-services::gc_service`)
/// before this call.
///
/// # Go registration order
///
/// The order follows Go's biz `fx_module.go` so the scheduler's
/// `List()` ordering matches:
///
/// 1. `ChannelService`           → `channel-model-sync`   (`channel.go:197`)
/// 2. `DataStorageService`       → `datastorage-fs-reload` (`data_storage.go:77-84`)
/// 3. `ChannelProbeService`      → `channel-probe`        (`channel_probe.go:71-78`)
/// 4. `PromptService`             → `prompt-cache`         (`prompt.go:69-76`)
/// 5. `ProviderQuotaService`     → `provider-quota-check` (`provider_quota.go:294-302`)
/// 6. `LiveStreamRegistry`        → sweeper goroutine      (`stream_preview.go:110-124`)
/// 7. `BackupService`             → `backup`               (`backup/service.go:38-46`)
/// 8. `video_storage.Worker`      → `video-storage`        (`worker.go:54-71`)
///
/// # Spawn gate
///
/// Each worker's loop is spawned only if `worker.enabled()` is true — disabled
/// workers are still REGISTERED (so `Scheduler.List()` parity holds with Go,
/// which always registers then short-circuits inside the callback) but no
/// tokio task is created for them. This matches Go's behavior where the cron
/// still fires but the callback body returns immediately on `!enabled`.
///
/// # Returns
///
/// The [`WorkerRuntime`] owning the join handles of the spawned loops. The
/// caller drives shutdown via the `shutdown` token.
///
/// # Errors
///
/// Returns [`SchedulerRegisterError`] if any `register_*` call fails (e.g.
/// a duplicate name, which would indicate a wiring bug). On error the scheduler
/// is left in a partially-registered state — the caller should drop it.
pub fn spawn_all_workers(
    scheduler: &mut Scheduler,
    shutdown: CancellationToken,
    defaults: &WorkerDefaults,
    now_provider: impl Fn() -> DateTime<Utc> + Clone + Send + 'static,
) -> Result<WorkerRuntime, SchedulerRegisterError> {
    let mut joins: Vec<JoinHandle<()>> = Vec::new();

    // 1. ChannelService → channel-model-sync (Go biz/fx_module.go:62-80 /
    //    channel.go:197-204).
    let model_sync = ChannelModelSyncWorker::new(
        ChannelModelSyncWorker::DEFAULT_NAME,
        defaults.auto_sync_frequency,
        defaults.enabled_model_sync,
    );
    let model_sync_enabled = model_sync.enabled();
    let model_sync = crate::worker::register_channel_model_sync_worker(scheduler, model_sync)?;
    if model_sync_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            model_sync,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 2. DataStorageService → datastorage-fs-reload (Go biz/fx_module.go:81-87
    //    / data_storage.go:77-84).
    let data_storage = DataStorageSyncWorker::new(
        DataStorageSyncWorker::DEFAULT_NAME,
        defaults.data_storage_interval,
        defaults.enabled_data_storage,
    );
    let data_storage_enabled = data_storage.enabled();
    let data_storage = crate::worker::register_data_storage_sync_worker(scheduler, data_storage)?;
    if data_storage_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            data_storage,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 3. ChannelProbeService → channel-probe (Go biz/fx_module.go:88-94 /
    //    channel_probe.go:71-78).
    let probe = ChannelProbeWorker::new(
        ChannelProbeWorker::DEFAULT_NAME,
        defaults.probe_frequency,
        defaults.enabled_probe,
    );
    let probe_enabled = probe.enabled();
    let probe = crate::worker::register_channel_probe_worker(scheduler, probe)?;
    if probe_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            probe,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 4. PromptService → prompt-cache (Go biz/fx_module.go:95-101 /
    //    prompt.go:69-76).
    let prompt_cache = PromptCacheWorker::new(
        PromptCacheWorker::DEFAULT_NAME,
        defaults.prompt_cache_interval,
        defaults.enabled_prompt_cache,
    );
    let prompt_cache_enabled = prompt_cache.enabled();
    let prompt_cache = crate::worker::register_prompt_cache_worker(scheduler, prompt_cache)?;
    if prompt_cache_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            prompt_cache,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 5. ProviderQuotaService → provider-quota-check (Go biz/fx_module.go:110-116
    //    / provider_quota.go:294-302).
    let provider_quota = ProviderQuotaCheckWorker::new(
        ProviderQuotaCheckWorker::DEFAULT_NAME,
        defaults.provider_quota_interval,
        defaults.enabled_provider_quota,
    );
    let provider_quota_enabled = provider_quota.enabled();
    let provider_quota =
        crate::worker::register_provider_quota_check_worker(scheduler, provider_quota)?;
    if provider_quota_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            provider_quota,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 6. LiveStreamRegistry → sweeper goroutine (Go biz/fx_module.go:45-61 /
    //    stream_preview.go:110-124). Go spawns this via `StartSweeper`, not
    //    `scheduler.Register`, but we project it through the same Worker shape
    //    so the trait-driven loop drives it uniformly.
    let sweeper = LiveStreamSweeperWorker::new(
        LiveStreamSweeperWorker::DEFAULT_NAME,
        defaults.live_stream_sweep_interval,
        defaults.enabled_live_stream_sweeper,
    );
    let sweeper_enabled = sweeper.enabled();
    let sweeper = crate::worker::register_live_stream_sweeper_worker(scheduler, sweeper)?;
    if sweeper_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            sweeper,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 7. BackupService → backup (Go backup/fx_module.go:13-19 /
    //    backup/service.go:38-46).
    let backup = AutoBackupWorker::new(
        AutoBackupWorker::DEFAULT_NAME,
        defaults.backup_frequency,
        defaults.enabled_backup,
    );
    let backup_enabled = backup.enabled();
    let backup = crate::worker::register_auto_backup_worker(scheduler, backup)?;
    if backup_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            backup,
            shutdown.clone(),
            now_provider.clone(),
        )));
    }

    // 8. video_storage.Worker → video-storage (Go video_storage/fx_module.go:13-19
    //    / worker.go:54-71).
    let video_storage = VideoStorageWorker::new(
        VideoStorageWorker::DEFAULT_NAME,
        defaults.video_storage_interval,
        defaults.enabled_video_storage,
    );
    let video_storage_enabled = video_storage.enabled();
    let video_storage = crate::worker::register_video_storage_worker(scheduler, video_storage)?;
    if video_storage_enabled {
        joins.push(tokio::spawn(run_worker_loop(
            video_storage,
            shutdown.clone(),
            now_provider,
        )));
    }

    Ok(WorkerRuntime { joins })
}

// ---------------------------------------------------------------------------
// Tests — assert the fx wiring mirrors Go's biz/backup/video_storage OnStart.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::Scheduler;
    use chrono::Utc;
    use std::time::Duration;

    /// All 8 workers register under their distinct canonical Go names, in
    /// Go's biz `fx_module.go` OnStart order.
    #[tokio::test]
    async fn spawn_all_registers_eight_distinct_named_workers()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        let defaults = WorkerDefaults {
            // Enable every worker so all 8 loops spawn — we shut them down
            // immediately afterwards.
            enabled_backup: true,
            ..WorkerDefaults::default()
        };

        let runtime = spawn_all_workers(&mut scheduler, shutdown.clone(), &defaults, Utc::now)?;

        // Registry holds exactly the 8 canonical names.
        assert_eq!(scheduler.registry().len(), 8);
        for name in WorkerRuntime::registered_names() {
            assert!(
                scheduler.registry().get(name).is_some(),
                "expected worker '{name}' to be registered"
            );
        }

        // All 8 names are distinct (no accidental dup wiring).
        let names = WorkerRuntime::registered_names();
        let unique = names
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(unique.len(), 8, "all 8 worker names must be distinct");

        // All 8 loops spawned because all are enabled.
        assert_eq!(runtime.spawned_count(), 8);

        // Clean shutdown — every loop terminates on cancel.
        runtime.shutdown(&shutdown).await;
        Ok(())
    }

    /// Disabled workers are registered (listing parity with Go) but NOT
    /// spawned. Mirrors Go's behavior where the cron still fires but the
    /// callback body returns immediately on `!enabled`.
    #[tokio::test]
    async fn disabled_workers_are_registered_but_not_spawned()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        let defaults = WorkerDefaults {
            // Disable everything.
            enabled_probe: false,
            enabled_model_sync: false,
            enabled_data_storage: false,
            enabled_provider_quota: false,
            enabled_live_stream_sweeper: false,
            enabled_prompt_cache: false,
            enabled_video_storage: false,
            enabled_backup: false,
            ..WorkerDefaults::default()
        };

        let runtime = spawn_all_workers(&mut scheduler, shutdown, &defaults, Utc::now)?;

        // All 8 registered (Go always registers, then short-circuits in the
        // callback body).
        assert_eq!(scheduler.registry().len(), 8);
        // None spawned.
        assert_eq!(runtime.spawned_count(), 0);
        assert!(runtime.joins.is_empty());
        Ok(())
    }

    /// Mixed enable/disable: enabled workers spawn, disabled ones don't, but
    /// all 8 register. Backup defaults to disabled (Go parity), the rest
    /// default to enabled.
    #[tokio::test]
    async fn default_switches_spawn_seven_loops_and_register_eight()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        let defaults = WorkerDefaults::default();

        // Sanity-check the default enable set before spawning.
        assert!(defaults.enabled_probe);
        assert!(defaults.enabled_model_sync);
        assert!(defaults.enabled_data_storage);
        assert!(defaults.enabled_provider_quota);
        assert!(defaults.enabled_live_stream_sweeper);
        assert!(defaults.enabled_prompt_cache);
        assert!(defaults.enabled_video_storage);
        assert!(
            !defaults.enabled_backup,
            "backup defaults to disabled (Go parity)"
        );

        let runtime = spawn_all_workers(&mut scheduler, shutdown.clone(), &defaults, Utc::now)?;

        // 8 registered, 7 spawned (all except backup).
        assert_eq!(scheduler.registry().len(), 8);
        assert_eq!(runtime.spawned_count(), 7);

        runtime.shutdown(&shutdown).await;
        Ok(())
    }

    /// The spawn-all function starts + stops cleanly with a fast tick interval
    /// + immediate shutdown, mirroring Go's fx OnStart/OnStop lifecycle. Uses
    /// 1-minute intervals (the data-storage / prompt-cache / video-storage
    /// default); the loops' first tick fires immediately (tokio default) but we
    /// cancel before any real work, so the test is fast.
    #[tokio::test]
    async fn spawn_all_starts_and_stops_cleanly_on_shutdown()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scheduler = Scheduler::new();
        let shutdown = CancellationToken::new();
        let defaults = WorkerDefaults {
            enabled_backup: true,
            ..WorkerDefaults::default()
        };

        let runtime = spawn_all_workers(&mut scheduler, shutdown.clone(), &defaults, Utc::now)?;
        assert_eq!(runtime.spawned_count(), 8);

        // Cancel after a tiny delay so loops have started.
        let cancel_clone = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cancel_clone.cancel();
        });

        // shutdown() awaits all joins — if any loop hangs, this test times out.
        runtime.shutdown(&shutdown).await;
        Ok(())
    }

    /// `WorkerRuntime::registered_names` returns the 8 canonical Go names in
    /// biz `fx_module.go` order.
    #[test]
    fn registered_names_match_go_canonical_order() {
        let names = [
            "channel-probe",
            "channel-model-sync",
            "datastorage-fs-reload",
            "provider-quota-check",
            "live-stream-sweeper",
            "prompt-cache",
            "video-storage",
            "backup",
        ];
        // Confirm the const DEFAULT_NAME values match the expected Go names.
        assert_eq!(ChannelProbeWorker::DEFAULT_NAME, names[0]);
        assert_eq!(ChannelModelSyncWorker::DEFAULT_NAME, names[1]);
        assert_eq!(DataStorageSyncWorker::DEFAULT_NAME, names[2]);
        assert_eq!(ProviderQuotaCheckWorker::DEFAULT_NAME, names[3]);
        assert_eq!(LiveStreamSweeperWorker::DEFAULT_NAME, names[4]);
        assert_eq!(PromptCacheWorker::DEFAULT_NAME, names[5]);
        assert_eq!(VideoStorageWorker::DEFAULT_NAME, names[6]);
        assert_eq!(AutoBackupWorker::DEFAULT_NAME, names[7]);

        // The static helper returns the same set.
        assert_eq!(WorkerRuntime::registered_names(), names);
    }

    /// `WorkerDefaults::default()` mirrors Go's per-service zero-value
    /// frequencies + the backup-disabled default.
    #[test]
    fn worker_defaults_match_go_zero_values() {
        let d = WorkerDefaults::default();
        // Frequencies match Go defaults (see WorkerDefaults doc table).
        assert_eq!(d.probe_frequency, ProbeFrequency::FiveMinutes);
        assert_eq!(d.auto_sync_frequency, AutoSyncFrequency::OneHour);
        assert_eq!(d.data_storage_interval, DataStorageReloadInterval::DEFAULT);
        assert_eq!(
            d.provider_quota_interval,
            ProviderQuotaCheckInterval::default()
        );
        assert_eq!(
            d.live_stream_sweep_interval,
            LiveStreamSweepInterval::DEFAULT
        );
        assert_eq!(d.prompt_cache_interval, PromptCacheReloadInterval::DEFAULT);
        assert_eq!(d.video_storage_interval, VideoStorageScanInterval::DEFAULT);
        assert_eq!(d.backup_frequency, BackupFrequency::Daily);
        // Backup defaults to disabled — matches Go's
        // `SchedulerWorkerSwitches::default().backup.enabled = false`.
        assert!(!d.enabled_backup);
    }
}

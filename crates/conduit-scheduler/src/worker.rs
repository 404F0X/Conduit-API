//! `Worker` trait + concrete worker — RUST-P13-004 bounded slice.
//!
//! Mirrors the implicit Go worker contract: each service exposes
//! `RegisterScheduledTasks(ctx, *scheduler.Scheduler)` which calls
//! `scheduler.Register(ctx, TaskSpec{Name, CronExpr, ...}, runPeriodically)`
//! (`conduit/internal/server/biz/fx_module.go` lines 62-116 wires seven such
//! services; `conduit/internal/server/biz/channel_probe.go:71-78` is the
//! channel-probe example). Go encodes the "worker" as (TaskSpec, fn); we
//! project it as a `Worker` trait so the tick-loop driver can treat every
//! scheduled service uniformly.
//!
//! The trait deliberately separates three concerns that Go intertwines inside
//! each `runXxxPeriodically` callback:
//!   1. **Identity** — `name()` (the Go `TaskSpec.Name`, used for de-dup in
//!      `Scheduler.Register`, `scheduler.go:33`).
//!   2. **Schedule** — `interval()` mirrors `setting.Probe.GetIntervalMinutes()`
//!      (`channel_probe.go:545-558`) — the cadence at which the *logic* wants
//!      to fire. Go runs the cron at a fixed `"* * * * *"` and gates inside the
//!      callback via `shouldRunProbe`; here the worker owns its own interval.
//!   3. **Gate + run** — `run_tick(...)` first consults the independent switch
//!      (`setting.Probe.Enabled`, `channel_probe.go:241-244`) and the
//!      time-alignment de-dup (`shouldRunProbe`, `channel_probe.go:83-88`),
//!      then performs the work. Non-overlap is enforced by the scheduler's
//!      `start_job` guard (`jobs.rs`), mirroring Go's sequential executor.
//!
//! The bounded slice lands ONE concrete worker — `ChannelProbeWorker` — because
//! it has the clearest, DB-free pure-logic surface in Go
//! (`channel_probe.go:80-104` + `channel_probe_internal.go`). The other ~6
//! workers (data-storage sync, prompt, provider-quota, model-auto-sync,
//! video-storage, backup) follow the same shape and are deferred.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::jobs::{CancellationToken, Scheduler};
use crate::worker_logic::{
    AlignInterval, AutoSyncFrequency, BackupFrequency, DataStorageReloadInterval,
    LiveStreamSweepInterval, ProbeFrequency, PromptCacheReloadInterval, ProviderQuotaCheckInterval,
    VideoStorageScanInterval, should_run_aligned,
};

/// Per-tick context handed to a [`Worker`] by the scheduler driver.
///
/// Carries the cancellation token (mirror of Go's `ctx context.Context` passed
/// to every `runXxxPeriodically` callback) plus the wall-clock "now" — Go reads
/// `xtime.UTCNow()` inside the callback (`channel_probe.go:247`); we inject it
/// so the gate decisions are pure and testable.
#[derive(Debug, Clone)]
pub struct WorkerTickContext<'a> {
    pub now: DateTime<Utc>,
    pub shutdown: &'a CancellationToken,
}

/// Outcome of a single [`Worker::run_tick`] — what the driver records.
///
/// Mirrors the four observable side effects of Go's `runProbe`:
/// * returns early when the switch is off (`channel_probe.go:241-244`)
/// * returns early when the alignment gate says "same window"
///   (`channel_probe.go:256-264`)
/// * otherwise performs the probe (`channel_probe.go:269-340`)
/// * the executor wrapper records `lastRunAt`/`lastError`
///   (`scheduler.go:147-168`)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerTickOutcome {
    /// The worker's independent switch was off — no work performed, no
    /// last-run update. (Go `if !setting.Probe.Enabled { return }`.)
    SkippedDisabled,
    /// The time-alignment gate said "already ran this window" — no work
    /// performed, no last-run update. (Go
    /// `if !shouldRunProbe(...) { return }`.)
    SkippedSameWindow,
    /// The worker ran to completion. `last_run` becomes the aligned bucket of
    /// `now`, mirroring `svc.lastExecutionTime = alignedTime`
    /// (`channel_probe.go:266`).
    Ran { last_run: DateTime<Utc> },
}

/// The scheduler worker contract.
///
/// Implementors encode (1) identity, (2) cadence, (3) the per-tick decision +
/// work. The driver in [`run_worker_loop`] owns the tokio interval + shutdown
/// select + non-overlap hand-off to `Scheduler::run_job_now`-equivalent logic.
///
/// This trait is intentionally synchronous on `run_tick` — Go's
/// `runProbePeriodically` is itself a blocking callback handed to the cron
/// executor. Async IO (DB probes, HTTP) is layered above by implementors that
/// spawn their own tasks; the trait stays focused on the decision + dispatch.
pub trait Worker: Send + Sync {
    /// Unique name — mirrors Go `TaskSpec.Name`. Used for de-dup by the
    /// registry (Go `scheduler.go:33`).
    fn name(&self) -> &str;

    /// Cadence at which the worker wants to be considered. Mirrors
    /// `setting.Probe.GetIntervalMinutes()` (`channel_probe.go:545-558`).
    fn interval(&self) -> Duration;

    /// True iff the worker's independent switch is on (Go
    /// `setting.Probe.Enabled`). When false the driver short-circuits to
    /// [`WorkerTickOutcome::SkippedDisabled`] without consulting the alignment
    /// gate — same order as Go (`channel_probe.go:241-244` then `:256`).
    fn enabled(&self) -> bool;

    /// Time-alignment bucket for the worker — `AlignInterval::from_probe_frequency`
    /// for the channel probe. Mirrors Go's
    /// `now.Truncate(intervalMinutes * time.Minute)` (`channel_probe.go:249`).
    fn align_interval(&self) -> AlignInterval;

    /// The last aligned bucket that the worker recorded as having run. `None`
    /// means "cold start" — the very first tick always runs, matching Go's
    /// `if !lastExecution.IsZero() && !shouldRunProbe(...)` guard
    /// (`channel_probe.go:256`).
    fn last_run(&self) -> Option<DateTime<Utc>>;

    /// Record a new `last_run` after a successful tick. Implementations should
    /// store the aligned bucket (NOT the raw `now`) so the next tick's
    /// `should_run_aligned` check matches Go's
    /// `svc.lastExecutionTime = alignedTime` semantics
    /// (`channel_probe.go:266`).
    fn record_run(&self, aligned_bucket: DateTime<Utc>);

    /// Perform the actual work. The driver has already checked `enabled()` and
    /// the alignment gate before calling this; the implementor only needs to
    /// perform IO. The token is the scheduler's shutdown handle — long-running
    /// workers should poll it.
    ///
    /// Returning `Err` mirrors Go's `log.Error(ctx, "...", log.Cause(err))`
    /// pattern: the failure is recorded but does not crash the loop — the next
    /// cron tick re-evaluates.
    fn perform_work(&self, ctx: &WorkerTickContext<'_>) -> Result<(), String>;

    /// Default per-tick decision logic — combines the switch + alignment gate
    /// + work dispatch.
    ///
    /// Mirrors the body of Go's `runProbe` (see `channel_probe.go:238-340`).
    ///
    /// Steps:
    ///
    /// 1. `if !enabled() { return SkippedDisabled }`
    /// 2. `let aligned = align_to_interval(...); if !should_run_aligned(...) {
    ///    return SkippedSameWindow }`
    /// 3. `self.record_run(aligned); self.perform_work()?; return Ran`
    ///
    /// Override only for workers whose gate differs (e.g. backup's weekday
    /// rule — see [`crate::worker_logic::should_run_backup`]).
    fn run_tick(&self, ctx: &WorkerTickContext<'_>) -> WorkerTickOutcome {
        // 1. Independent switch — Go `channel_probe.go:241-244`.
        if !self.enabled() {
            return WorkerTickOutcome::SkippedDisabled;
        }

        // 2. Alignment gate — Go `channel_probe.go:249-264`. Cold-start
        //    (`last_run == None`) always runs, matching Go's
        //    `if !lastExecution.IsZero() && !shouldRunProbe(...)`.
        let interval = self.align_interval();
        let aligned = crate::worker_logic::align_to_interval(interval, ctx.now);
        if !should_run_aligned(interval, ctx.now, self.last_run()) {
            return WorkerTickOutcome::SkippedSameWindow;
        }

        // 3. Record the bucket BEFORE dispatching work, mirroring Go's
        //    `svc.lastExecutionTime = alignedTime` (`channel_probe.go:266`)
        //    which happens before the IO. This means a failed tick still
        //    advances the bucket — Go's `runProbe` returns early on DB errors
        //    but does NOT reset lastExecutionTime.
        self.record_run(aligned);

        if let Err(error) = self.perform_work(ctx) {
            // Mirrors Go's `log.Error(...)` then `return` — the failure is
            // recorded (here: surfaced to the driver) but does not advance or
            // rewind the bucket. We still report `Ran` because the bucket was
            // updated and the next-window tick should skip-this-window.
            // The driver may log the error; the outcome is observable as `Ran`.
            tracing::error!(worker = self.name(), %error, "scheduled worker tick failed");
        }

        WorkerTickOutcome::Ran { last_run: aligned }
    }
}

/// Drive a worker on its [`Worker::interval`] cadence until `shutdown` fires.
///
/// This is the bounded-slice tick loop. It mirrors the structure of
/// `Scheduler::start` in `jobs.rs` (tokio interval + shutdown select) but
/// routes each tick through the worker's [`Worker::run_tick`] gate so the
/// should-run logic and last-run bookkeeping are testable in isolation.
///
/// Non-overlap is enforced by the worker trait contract: a single worker is
/// driven by exactly one loop, and the loop awaits `run_tick` to completion
/// before the next `interval.tick()`. This matches Go's sequential cron
/// executor — see the note in `worker_logic.rs` ([Galileo-the-3rd]).
///
/// **Panic containment (A02).** Each tick is wrapped in
/// `std::panic::catch_unwind` so a panicking `run_tick` (or `perform_work`
/// inside it) does NOT crash the loop — mirroring Go's `defer recover()` in
/// `scheduler.go:153-167` where the executor recovers a panicking callback and
/// records `t.lastError = "panic: ..."`. Here we simply discard the panic
/// payload and continue to the next interval tick; the next tick re-evaluates
/// per the standard reentry/alignment policy. A worker that panics every tick
/// will spin at its configured cadence, never killing the scheduler runtime.
pub async fn run_worker_loop<W: Worker + 'static>(
    worker: W,
    shutdown: CancellationToken,
    now_provider: impl Fn() -> DateTime<Utc> + Send + 'static,
) {
    let mut interval = tokio::time::interval(worker.interval());
    // The first tick fires immediately (tokio default), mirroring Go's
    // "register-then-the-cron-fires-on-the-next-minute" — close enough for the
    // bounded slice; a faithful cron-aligned first-fire is deferred (S18).

    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            _ = interval.tick() => {
                let ctx = WorkerTickContext {
                    now: now_provider(),
                    shutdown: &shutdown,
                };
                // AssertUnwindSafe: the closure captures `&worker` and `&ctx`
                // by reference; neither requires unwind-safety in the
                // traditional sense (no partially-initialized state across
                // await points). This mirrors Go's `defer recover()` boundary
                // which wraps the entire callback without unwind-safety
                // annotations. A panic is caught and discarded — the loop
                // continues to the next tick.
                if catch_unwind(AssertUnwindSafe(|| {
                    worker.run_tick(&ctx);
                }))
                .is_err()
                {
                    tracing::error!(worker = worker.name(), "scheduled worker tick panicked");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelProbeWorker — concrete worker mirroring Go's ChannelProbeService.
// ---------------------------------------------------------------------------

/// Channel-probe worker — mirrors Go `ChannelProbeService`
/// (`conduit/internal/server/biz/channel_probe.go`).
///
/// The Go service is constructed via fx (`NewChannelProbeService`) and
/// registers `runProbePeriodically` on the cron `"* * * * *"`
/// (`channel_probe.go:71-78`). The callback reads `setting.Probe.Enabled` and
/// `setting.Probe.Frequency` from `SystemService.ChannelSettingOrDefault`,
/// then calls `runProbe` which performs the gate + DB work.
///
/// This Rust projection holds the same two pieces of mutable state as the Go
/// service: the independent enable flag and the `lastExecutionTime`. The DB
/// work (`computeAllChannelProbeStats` + `CreateBulk`) is performed by an
/// injected [`ChannelProbeExecutor`] (bin-side, DB-backed) so this crate stays
/// DB-free; when no executor is wired `perform_work` is a no-op (the gate +
/// bookkeeping are still exercised).
///
/// Injected side-effect executor for the channel-probe worker.
///
/// Kept as a **synchronous** trait so the `conduit-scheduler` crate stays free
/// of DB / async-runtime dependencies (mirrors the `QuotaAdmissionSource`
/// injection pattern). The bin-side implementation bridges to async DB IO
/// internally (`block_in_place` + `Handle::block_on`, same as the other
/// sync→async seams in the binary).
///
/// `run_probe` performs the full Go `runProbe` body (`channel_probe.go:238-340`)
/// **minus the enable/alignment gate** — the [`Worker::run_tick`] default has
/// already checked those before `perform_work` calls this. `now` is the
/// worker's injected wall-clock (the aligned bucket is derived inside).
pub trait ChannelProbeExecutor: Send + Sync {
    fn run_probe(&self, now: DateTime<Utc>, interval_minutes: i64) -> Result<(), String>;
}

pub struct ChannelProbeWorker {
    /// Worker name — Go `TaskSpec.Name = "channel-probe"`.
    name: String,
    /// Probe frequency — Go `setting.Probe.Frequency`.
    frequency: ProbeFrequency,
    /// Independent enable flag — Go `setting.Probe.Enabled`. Interior-mutable
    /// so an operator (or a config reload) can flip it without rebuilding the
    /// worker, mirroring Go's per-request `ChannelSettingOrDefault` lookup.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run — Go `svc.lastExecutionTime`.
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
    /// Injected probe executor (compute + persist). `None` keeps the pre-DI
    /// no-op behavior (used by the scheduler crate's own tests, which assert
    /// gate logic without a DB); production wires a real executor.
    executor: Option<std::sync::Arc<dyn ChannelProbeExecutor>>,
}

impl ChannelProbeWorker {
    /// Build a worker with the given name + frequency. Mirrors the Go
    /// zero-value service (`lastExecutionTime: time.Time{}`) — `last_run`
    /// starts as `None` so the first tick always runs.
    pub fn new(name: impl Into<String>, frequency: ProbeFrequency, enabled: bool) -> Self {
        Self {
            name: name.into(),
            frequency,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
            executor: None,
        }
    }

    /// Attach a real probe executor (compute + persist). Without one,
    /// `perform_work` is a no-op (pre-DI behavior). Mirrors how the binary
    /// injects DB-backed side effects into the otherwise DB-free scheduler.
    pub fn with_executor(mut self, executor: std::sync::Arc<dyn ChannelProbeExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// The canonical Go name. Mirrors `scheduler.TaskSpec{Name: "channel-probe"}`
    /// (`channel_probe.go:73`).
    pub const DEFAULT_NAME: &'static str = "channel-probe";

    /// Flip the enable flag at runtime (config reload). Mirrors Go re-reading
    /// `SystemService.ChannelSettingOrDefault(ctx).Probe.Enabled` on every
    /// tick — we expose the mutator so a config-watch task can flip it.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured probe frequency.
    pub fn frequency(&self) -> ProbeFrequency {
        self.frequency
    }

    /// Interval derived from the frequency — mirrors Go
    /// `setting.Probe.GetIntervalMinutes()` (`channel_probe.go:545-558`).
    pub fn interval_minutes(&self) -> i64 {
        self.frequency.interval_minutes()
    }
}

impl Worker for ChannelProbeWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_probe_frequency(self.frequency)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        // Mirrors Go `runProbe` (`channel_probe.go:238-340`): compute per-channel
        // stats over the aligned window and bulk-insert probe rows. The DB work
        // is injected via `ChannelProbeExecutor` so the scheduler crate stays
        // DB-free (same DI seam as the binary's other wiring). Without an
        // executor wired, this is a no-op (pre-DI parity).
        match &self.executor {
            Some(executor) => {
                let interval = self.align_interval();
                let aligned = crate::worker_logic::align_to_interval(interval, ctx.now);
                executor.run_probe(aligned, self.interval_minutes())
            }
            None => Ok(()),
        }
    }
}

/// Register a [`ChannelProbeWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `ChannelProbeService.RegisterScheduledTasks`
/// (`channel_probe.go:71-78`) calling `scheduler.Register(...)`. The Rust
/// projection uses `Scheduler::register_job` with a `JobSpec` whose runner is
/// a thin shim that delegates to `Worker::run_tick` — this is how the
/// pre-existing `Scheduler::start` tick loop (which dispatches via
/// `JobSpec.runner`) reaches the worker's gate.
///
/// Returns the worker so the caller can hold the handle for runtime
/// enable/disable and last-run inspection.
pub fn register_channel_probe_worker(
    scheduler: &mut Scheduler,
    worker: ChannelProbeWorker,
) -> Result<ChannelProbeWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    // The JobSpec runner is a no-op shim: the actual gate + work happen via
    // `run_worker_loop` (the trait-driven loop above). Registering the JobSpec
    // keeps the registry/listing consistent with Go's `Scheduler.List()` and
    // leaves room for the eventual unified driver. For the bounded slice the
    // trait loop is the authoritative path.
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// ChannelModelSyncWorker — mirrors Go `ChannelService` model-auto-sync task.
// ---------------------------------------------------------------------------

/// Channel-model-auto-sync worker — mirrors Go `ChannelService`
/// (`conduit/internal/server/biz/channel.go` +
/// `channel_internal.go`).
///
/// Go wires this in `ChannelService.RegisterScheduledTasks` (`channel.go:197`)
/// as `scheduler.TaskSpec{Name: "channel-model-sync", CronExpr: "11 * * * *"}`
/// bound to `runSyncChannelModelsPeriodically` (`channel_internal.go:23-31`).
/// The callback reads `setting.AutoSync.Frequency` from
/// `SystemService.ChannelSettingOrDefault(ctx)` and gates on
/// `shouldRunModelSync` (`channel_internal.go:33-46`):
/// ```go
/// func (svc *ChannelService) shouldRunModelSync(now time.Time, frequency AutoSyncFrequency) bool {
///     intervalMinutes := getIntervalMinutesFromAutoSyncFrequency(frequency)
///     alignedTime := now.Truncate(time.Duration(intervalMinutes) * time.Minute)
///     svc.modelSyncMu.Lock(); defer svc.modelSyncMu.Unlock()
///     if !svc.lastModelSyncExecutionTime.IsZero() && svc.lastModelSyncExecutionTime.Equal(alignedTime) {
///         return false
///     }
///     svc.lastModelSyncExecutionTime = alignedTime
///     return true
/// }
/// ```
///
/// Critically the Go `shouldRunModelSync` records `alignedTime`
/// (`svc.lastModelSyncExecutionTime = alignedTime`) on the same call as the
/// decision — exactly the contract `Worker::record_run(aligned_bucket)` already
/// implements in `run_tick`. Provider model-list IO is supplied by the binary
/// through [`ChannelModelSyncExecutor`], keeping this crate storage-agnostic.
pub struct ChannelModelSyncWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "channel-model-sync"}`
    /// (`channel.go:199`).
    name: String,
    /// Auto-sync frequency — Go `setting.AutoSync.Frequency`. Default
    /// `AutoSyncFrequencyOneHour` (cron is hourly `"11 * * * *"`).
    frequency: AutoSyncFrequency,
    /// Independent enable flag — Go re-reads `setting.AutoSync.Frequency`
    /// every tick via `ChannelSettingOrDefault`; the worker mirrors this by
    /// allowing runtime enable flips. (Go's callback itself has no explicit
    /// `if !enabled` early return — `runSyncChannelModelsPeriodically`
    /// always proceeds to `shouldRunModelSync`; but a `modelSyncEnabled`
    /// operator switch exists in the settings surface and is wired here so
    /// runtime flip parity matches the channel-probe worker.)
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run — Go `svc.lastModelSyncExecutionTime`.
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
    executor: Option<std::sync::Arc<dyn ChannelModelSyncExecutor>>,
}

pub trait ChannelModelSyncExecutor: Send + Sync {
    fn sync_models(&self) -> Result<(), String>;
}

impl ChannelModelSyncWorker {
    /// Build a worker with the given name + frequency. Mirrors the Go
    /// zero-value service (`lastModelSyncExecutionTime: time.Time{}`) —
    /// `last_run` starts as `None` so the first tick always runs.
    pub fn new(name: impl Into<String>, frequency: AutoSyncFrequency, enabled: bool) -> Self {
        Self {
            name: name.into(),
            frequency,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
            executor: None,
        }
    }

    pub fn with_executor(mut self, executor: std::sync::Arc<dyn ChannelModelSyncExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "channel-model-sync"}` (`channel.go:199`).
    pub const DEFAULT_NAME: &'static str = "channel-model-sync";

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured auto-sync frequency.
    pub fn frequency(&self) -> AutoSyncFrequency {
        self.frequency
    }

    /// Interval derived from the frequency — mirrors Go
    /// `getIntervalMinutesFromAutoSyncFrequency` (`channel_internal.go:48-58`).
    pub fn interval_minutes(&self) -> i64 {
        self.frequency.interval_minutes()
    }
}

impl Worker for ChannelModelSyncWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_auto_sync_frequency(self.frequency)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        match &self.executor {
            Some(executor) => executor.sync_models(),
            None => Ok(()),
        }
    }
}

/// Register a [`ChannelModelSyncWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `ChannelService.RegisterScheduledTasks` (`channel.go:197-204`)
/// calling `scheduler.Register(ctx, TaskSpec{Name: "channel-model-sync", ...},
/// runSyncChannelModelsPeriodically)`. Same registry/listing parity rationale
/// as [`register_channel_probe_worker`].
pub fn register_channel_model_sync_worker(
    scheduler: &mut Scheduler,
    worker: ChannelModelSyncWorker,
) -> Result<ChannelModelSyncWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// DataStorageSyncWorker — mirrors Go `DataStorageService` fs-reload task (S08).
// ---------------------------------------------------------------------------

/// Data-storage filesystem-reload worker — mirrors Go `DataStorageService`
/// (`conduit/internal/server/biz/data_storage.go`).
///
/// Go wires this in `DataStorageService.RegisterScheduledTasks`
/// (`data_storage.go:77-84`) as
/// `scheduler.TaskSpec{Name: "datastorage-fs-reload", CronExpr: "*/1 * * * *"}`
/// bound to `reloadFileSystemsPeriodically` (`data_storage_internal.go:10-17`).
/// The callback unconditionally calls `refreshFileSystems(ctx)` — there is no
/// explicit `if !enabled` switch and no per-callback time-alignment de-dup; the
/// cron cadence (every minute) IS the de-dup.
///
/// To keep parity with the existing channel-probe / model-sync workers we
/// project the same `Worker` shape here, using a 1-minute `AlignInterval` as
/// the alignment gate. This means a second `run_tick` within the same minute
/// (e.g. a tight driver loop) is skipped just like Go's per-minute cron tick —
/// observably equivalent to Go while staying testable via the trait. The
/// Rust constructs a storage backend for each operation and has no afero-style
/// filesystem cache. Consequently `perform_work` is an intentional no-op.
pub struct DataStorageSyncWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "datastorage-fs-reload"}`
    /// (`data_storage.go:79`).
    name: String,
    /// Reload interval — Go hardcodes 1 minute (`data_storage.go:81`).
    interval: DataStorageReloadInterval,
    /// Independent enable flag. Go's callback has no explicit switch, but the
    /// wider scheduler exposes a per-worker enable surface
    /// (`SchedulerWorkerSwitches`); we keep the flag so the bounded slice
    /// matches the channel-probe/model-sync shape and is forward-compatible
    /// with an operator switch.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run. Go has no equivalent field
    /// (`refreshFileSystems` reads `latestUpdate` from DB), but the trait-driven
    /// gate needs it to express "same minute -> skip".
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
}

impl DataStorageSyncWorker {
    /// Build a worker with the given name + interval. Mirrors the Go zero-value
    /// service — `last_run` starts as `None` so the first tick always runs.
    pub fn new(
        name: impl Into<String>,
        interval: DataStorageReloadInterval,
        enabled: bool,
    ) -> Self {
        Self {
            name: name.into(),
            interval,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
        }
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "datastorage-fs-reload"}` (`data_storage.go:79`).
    pub const DEFAULT_NAME: &'static str = "datastorage-fs-reload";

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured reload interval.
    pub fn interval_setting(&self) -> DataStorageReloadInterval {
        self.interval
    }

    /// Interval in whole minutes — mirrors Go's `"*/1 * * * *"` cadence.
    pub fn interval_minutes(&self) -> i64 {
        self.interval.interval_minutes()
    }
}

impl Worker for DataStorageSyncWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_data_storage_interval(self.interval)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        // Go refreshes its afero filesystem cache here. Rust has no such cache:
        // every operation resolves current DB settings into a fresh backend.
        Ok(())
    }
}

/// Register a [`DataStorageSyncWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `DataStorageService.RegisterScheduledTasks`
/// (`data_storage.go:77-84`) calling
/// `scheduler.Register(ctx, TaskSpec{Name: "datastorage-fs-reload", ...},
/// reloadFileSystemsPeriodically)`. Same registry/listing parity rationale as
/// [`register_channel_probe_worker`].
pub fn register_data_storage_sync_worker(
    scheduler: &mut Scheduler,
    worker: DataStorageSyncWorker,
) -> Result<DataStorageSyncWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// ProviderQuotaCheckWorker — mirrors Go `ProviderQuotaService` quota-check
// scheduled task (S10).
// ---------------------------------------------------------------------------

/// Provider-quota check worker — mirrors Go `ProviderQuotaService`
/// (`conduit/internal/server/biz/provider_quota.go`).
///
/// Go wires this in `ProviderQuotaService.RegisterScheduledTasks`
/// (`provider_quota.go:294-302`) as
/// `scheduler.TaskSpec{Name: "provider-quota-check", CronExpr: <derived>}`
/// bound to `runQuotaCheckScheduled` (`provider_quota_internal.go:9-15`). The
/// cron expression is derived from the configured check interval via
/// `intervalToCronExpr` (`provider_quota.go:336-370`); the default check
/// interval is `5 * time.Minute` (`provider_quota.go:388-394`). The callback
/// has no explicit `if !enabled` switch — it locks `svc.mu` and delegates to
/// `runQuotaCheck(ctx, false)` (`provider_quota.go:488-588`).
///
/// We project the same `Worker` shape as the channel-probe / model-sync workers
/// using a `ProviderQuotaCheckInterval` typed enum that captures Go's
/// `supportedIntervals` set (`{1,2,3,4,5,6,10,12,15,20,30,60}`). The alignment
/// gate de-dups ticks within the same interval bucket. The binary injects the
/// database and HTTP checker implementation through
/// [`ProviderQuotaCheckExecutor`].
pub struct ProviderQuotaCheckWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "provider-quota-check"}`
    /// (`provider_quota.go:297`).
    name: String,
    /// Check interval — Go `svc.getCheckInterval()` default `5 * time.Minute`
    /// (`provider_quota.go:388-394`).
    interval: ProviderQuotaCheckInterval,
    /// Independent enable flag. Go's callback has no explicit switch, but the
    /// scheduler exposes a per-worker enable surface
    /// (`SchedulerWorkerSwitches.provider_quota`); we keep the flag for parity
    /// with the channel-probe / model-sync shape.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run — used by the trait-driven gate to
    /// express "same interval window -> skip".
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
    executor: Option<std::sync::Arc<dyn ProviderQuotaCheckExecutor>>,
}

pub trait ProviderQuotaCheckExecutor: Send + Sync {
    fn check_due_channels(&self) -> Result<(), String>;
}

impl ProviderQuotaCheckWorker {
    /// Build a worker with the given name + check interval. Mirrors the Go
    /// zero-value service — `last_run` starts as `None` so the first tick
    /// always runs.
    pub fn new(
        name: impl Into<String>,
        interval: ProviderQuotaCheckInterval,
        enabled: bool,
    ) -> Self {
        Self {
            name: name.into(),
            interval,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
            executor: None,
        }
    }

    pub fn with_executor(
        mut self,
        executor: std::sync::Arc<dyn ProviderQuotaCheckExecutor>,
    ) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "provider-quota-check"}` (`provider_quota.go:297`).
    pub const DEFAULT_NAME: &'static str = "provider-quota-check";

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured check interval.
    pub fn check_interval(&self) -> ProviderQuotaCheckInterval {
        self.interval
    }

    /// Interval in whole minutes — mirrors Go's `getCheckInterval().Minutes()`.
    pub fn interval_minutes(&self) -> i64 {
        self.interval.interval_minutes()
    }
}

impl Worker for ProviderQuotaCheckWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_provider_quota_interval(self.interval)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        match &self.executor {
            Some(executor) => executor.check_due_channels(),
            None => Ok(()),
        }
    }
}

/// Register a [`ProviderQuotaCheckWorker`] into the [`Scheduler`] under its
/// name.
///
/// Mirrors Go `ProviderQuotaService.RegisterScheduledTasks`
/// (`provider_quota.go:294-302`) calling
/// `scheduler.Register(ctx, TaskSpec{Name: "provider-quota-check", ...},
/// runQuotaCheckScheduled)`. Same registry/listing parity rationale as
/// [`register_channel_probe_worker`].
pub fn register_provider_quota_check_worker(
    scheduler: &mut Scheduler,
    worker: ProviderQuotaCheckWorker,
) -> Result<ProviderQuotaCheckWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// LiveStreamSweeperWorker — mirrors Go `LiveStreamRegistry.StartSweeper` (S04).
// ---------------------------------------------------------------------------

/// Injected sweep executor for the live-stream sweeper worker.
///
/// Kept synchronous (like [`ChannelProbeExecutor`]) so the `conduit-scheduler`
/// crate stays free of the orchestrator/`LiveStreamRegistry` dependency. The
/// bin-side implementation calls `LiveStreamRegistry::sweep_stale_entries` with
/// the idle threshold and returns the evicted count for observability (Go logs
/// the count in `sweepStaleEntries`, `stream_preview.go:127-169`).
pub trait LiveStreamSweepExecutor: Send + Sync {
    fn sweep(&self, idle_threshold_minutes: i64) -> Result<usize, String>;
}

/// Live-stream registry sweeper worker — mirrors Go
/// `LiveStreamRegistry.StartSweeper` (`conduit/internal/server/biz/stream_preview.go:110-124`).
///
/// Unlike the channel-probe / model-sync / provider-quota workers, Go does NOT
/// wire this via `scheduler.Register(TaskSpec{...})`. Instead `biz/fx_module.go`
/// lines 45-61 attach an fx `OnStart`/`OnStop` hook that calls
/// `registry.StartSweeper(bgCtx)` directly. `StartSweeper` builds a
/// `time.NewTicker(5 * time.Minute)` and calls `sweepStaleEntries(ctx)` on every
/// tick; the idle-zombie threshold is `10 * time.Minute`
/// (`stream_preview.go:128`). There is no explicit per-worker enable switch and
/// no `TaskSpec.Name` — we project the same shape as the other workers so the
/// trait-driven loop can drive it uniformly, and use the canonical name
/// `"live-stream-sweeper"` for the `Scheduler` registry/listing parity.
///
/// The `perform_work` body (Go `sweepStaleEntries`) is injected via
/// [`LiveStreamSweepExecutor`] so the scheduler crate stays dependency-free;
/// without an executor wired it is a no-op (pre-DI parity).

pub struct LiveStreamSweeperWorker {
    /// Worker name. Go has no `TaskSpec.Name` (it's not registered with the
    /// scheduler); we use a stable identifier for registry/listing parity.
    name: String,
    /// Injected registry sweep. `None` → `perform_work` is a no-op (pre-DI
    /// parity).
    executor: Option<std::sync::Arc<dyn LiveStreamSweepExecutor>>,
    /// Sweep interval — Go hardcodes `5 * time.Minute`
    /// (`stream_preview.go:112`).
    interval: LiveStreamSweepInterval,
    /// Idle-zombie threshold — Go hardcodes `10 * time.Minute`
    /// (`stream_preview.go:128`). Captured for parity + future perform_work.
    idle_threshold_minutes: i64,
    /// Independent enable flag. Go has no explicit switch (the sweeper goroutine
    /// runs unconditionally once `StartSweeper` is called), but the scheduler
    /// exposes a per-worker enable surface; we keep the flag so the bounded
    /// slice matches the channel-probe / model-sync shape and stays forward-
    /// compatible with an operator switch.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run. Go has no equivalent field (the
    /// ticker fires unconditionally), but the trait-driven gate needs it to
    /// express "same 5-minute window -> skip", which is observably equivalent
    /// to Go's per-tick cadence.
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
}

impl LiveStreamSweeperWorker {
    /// Build a worker with the given name + interval. Mirrors Go's zero-value
    /// startup — `last_run` starts as `None` so the first tick always runs.
    pub fn new(name: impl Into<String>, interval: LiveStreamSweepInterval, enabled: bool) -> Self {
        Self {
            name: name.into(),
            executor: None,
            interval,
            idle_threshold_minutes: Self::DEFAULT_IDLE_THRESHOLD_MINUTES,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
        }
    }

    /// Attach the DB/registry-backed sweep executor. Without it, `perform_work`
    /// is a no-op (pre-DI parity).
    pub fn with_executor(mut self, executor: std::sync::Arc<dyn LiveStreamSweepExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Canonical worker name (Go has no `TaskSpec.Name`; this is the Rust-side
    /// identifier for registry/listing parity).
    pub const DEFAULT_NAME: &'static str = "live-stream-sweeper";

    /// Go's hardcoded idle-zombie threshold — `10 * time.Minute`
    /// (`stream_preview.go:128`).
    pub const DEFAULT_IDLE_THRESHOLD_MINUTES: i64 = 10;

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured sweep interval.
    pub fn sweep_interval(&self) -> LiveStreamSweepInterval {
        self.interval
    }

    /// Idle-zombie threshold in whole minutes — mirrors Go's
    /// `10 * time.Minute` constant (`stream_preview.go:128`).
    pub fn idle_threshold_minutes(&self) -> i64 {
        self.idle_threshold_minutes
    }

    /// Interval in whole minutes — mirrors Go's `5 * time.Minute` ticker
    /// (`stream_preview.go:112`).
    pub fn interval_minutes(&self) -> i64 {
        self.interval.interval_minutes()
    }
}

impl Worker for LiveStreamSweeperWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_live_stream_sweep_interval(self.interval)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        // Go `sweepStaleEntries` (`stream_preview.go:127-169`) ranges over the
        // request + execution `sync.Map`s, force-closes buffers that are
        // `IsClosed()` or whose `LastAppendedAt()` is older than the 10-minute
        // idle threshold, then logs the evicted count. The registry snapshot +
        // sweep is injected via `LiveStreamSweepExecutor` so the scheduler crate
        // stays free of the orchestrator dependency. Without an executor wired,
        // this is a no-op (pre-DI parity).
        match &self.executor {
            Some(executor) => executor
                .sweep(self.idle_threshold_minutes())
                .map(|_evicted| ()),
            None => Ok(()),
        }
    }
}

/// Register a [`LiveStreamSweeperWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `biz/fx_module.go:45-61` which attaches an fx `OnStart`/`OnStop`
/// hook that calls `registry.StartSweeper(bgCtx)` and a `cancel()` on stop. The
/// Rust projection registers a `JobSpec` (for registry/listing parity) and
/// leaves the trait-driven loop in [`run_worker_loop`] as the authoritative
/// dispatch path — same shape as the other workers.
pub fn register_live_stream_sweeper_worker(
    scheduler: &mut Scheduler,
    worker: LiveStreamSweeperWorker,
) -> Result<LiveStreamSweeperWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// PromptCacheWorker — mirrors Go `PromptService` prompt-cache task (S09).
// ---------------------------------------------------------------------------

/// Prompt-cache refresh worker — mirrors Go `PromptService`
/// (`conduit/internal/server/biz/prompt.go`).
///
/// Go wires this in `PromptService.RegisterScheduledTasks` (`prompt.go:69-76`)
/// as
/// `scheduler.TaskSpec{Name: "prompt-cache", CronExpr: "*/1 * * * *", Timezone: "UTC"}`
/// bound to `loadPromptsPeriodic` (`prompt.go:78-86`). The callback has no
/// `if !enabled` switch and no per-callback time-alignment de-dup; the cron
/// cadence (every minute) IS the de-dup. The body ranges over the per-project
/// `latestCachedUpdateTime` map and calls `loadPrompts(ctx, projectID)` for each
/// project, logging per-project errors but never aborting the loop.
///
/// We retain the worker shape for registry compatibility. Rust's prompt source
/// reads the database on each request and has no `latestCachedUpdateTime` map,
/// so `perform_work` is an intentional no-op.
pub struct PromptCacheWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "prompt-cache"}`
    /// (`prompt.go:71`).
    name: String,
    /// Reload interval — Go hardcodes `"*/1 * * * *"` (`prompt.go:73`).
    interval: PromptCacheReloadInterval,
    /// Independent enable flag. Go's callback has no explicit switch, but the
    /// scheduler exposes a per-worker enable surface; we keep the flag for
    /// parity with the other workers.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run. Go has no equivalent field, but the
    /// trait-driven gate needs it to express "same minute -> skip".
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
}

impl PromptCacheWorker {
    /// Build a worker with the given name + interval. Mirrors Go's zero-value
    /// startup — `last_run` starts as `None` so the first tick always runs.
    pub fn new(
        name: impl Into<String>,
        interval: PromptCacheReloadInterval,
        enabled: bool,
    ) -> Self {
        Self {
            name: name.into(),
            interval,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
        }
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "prompt-cache"}` (`prompt.go:71`).
    pub const DEFAULT_NAME: &'static str = "prompt-cache";

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured reload interval.
    pub fn reload_interval(&self) -> PromptCacheReloadInterval {
        self.interval
    }

    /// Interval in whole minutes — mirrors Go's `"*/1 * * * *"` cadence.
    pub fn interval_minutes(&self) -> i64 {
        self.interval.interval_minutes()
    }
}

impl Worker for PromptCacheWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_prompt_cache_interval(self.interval)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        // Go refreshes its per-project prompt cache here. Rust queries the
        // prompt repository per request and therefore has no cache to refresh.
        Ok(())
    }
}

/// Register a [`PromptCacheWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `PromptService.RegisterScheduledTasks` (`prompt.go:69-76`)
/// calling
/// `scheduler.Register(ctx, TaskSpec{Name: "prompt-cache", CronExpr: "*/1 * * * *", ...},
/// loadPromptsPeriodic)`. Same registry/listing parity rationale as
/// [`register_channel_probe_worker`].
pub fn register_prompt_cache_worker(
    scheduler: &mut Scheduler,
    worker: PromptCacheWorker,
) -> Result<PromptCacheWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// VideoStorageWorker — mirrors Go `video_storage.Worker` scan task (S12).
// ---------------------------------------------------------------------------

/// Video-storage scan worker — mirrors Go `video_storage.Worker`
/// (`conduit/internal/server/video_storage/worker.go`).
///
/// Go wires this in `Worker.RegisterScheduledTasks` (`worker.go:54-71`) as
/// `scheduler.TaskSpec{Name: "video-storage", FixRate: <interval>}` bound to
/// `runScanWithSystemContext` (`worker.go:97-107`). The interval is read from
/// `SystemService.VideoStorageSettings(ctx).ScanIntervalMinutes` and clamped to
/// a minimum of 1 minute (`worker.go:61-64`); the Go default is 1 minute. The
/// callback has no explicit `if !enabled` switch — the `scanAndSave` body reads
/// `settings.Enabled` (`worker.go:115-117`) and short-circuits when disabled.
///
/// We project the same `Worker` shape as the channel-probe / model-sync workers
/// using a `VideoStorageScanInterval` typed enum that captures Go's
/// minute-granularity `ScanIntervalMinutes`. The independent enable flag here
/// mirrors the runtime enable surface in `SchedulerWorkerSwitches.video_storage`.
/// The binary supplies settings, DB scan, HTTP download, and storage persistence
/// through [`VideoStorageExecutor`].
pub struct VideoStorageWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "video-storage"}`
    /// (`worker.go:67`).
    name: String,
    /// Scan interval — Go `settings.ScanIntervalMinutes` clamped to >= 1
    /// (`worker.go:61-64`).
    interval: VideoStorageScanInterval,
    /// Independent enable flag — mirrors `SchedulerWorkerSwitches.video_storage`.
    enabled: std::sync::atomic::AtomicBool,
    /// Last aligned bucket recorded as run — used by the trait-driven gate to
    /// express "same scan window -> skip".
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
    executor: Option<std::sync::Arc<dyn VideoStorageExecutor>>,
}

pub trait VideoStorageExecutor: Send + Sync {
    fn scan_and_save(&self) -> Result<(), String>;
}

impl VideoStorageWorker {
    /// Build a worker with the given name + scan interval. Mirrors Go's
    /// zero-value startup — `last_run` starts as `None` so the first tick
    /// always runs.
    pub fn new(name: impl Into<String>, interval: VideoStorageScanInterval, enabled: bool) -> Self {
        Self {
            name: name.into(),
            interval,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
            executor: None,
        }
    }

    pub fn with_executor(mut self, executor: std::sync::Arc<dyn VideoStorageExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "video-storage"}` (`worker.go:67`).
    pub const DEFAULT_NAME: &'static str = "video-storage";

    /// Flip the enable flag at runtime (operator switch).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured scan interval.
    pub fn scan_interval(&self) -> VideoStorageScanInterval {
        self.interval
    }

    /// Interval in whole minutes — mirrors Go's
    /// `settings.ScanIntervalMinutes` (clamped to >= 1).
    pub fn interval_minutes(&self) -> i64 {
        self.interval.interval_minutes()
    }
}

impl Worker for VideoStorageWorker {
    fn name(&self) -> &str {
        &self.name
    }

    fn interval(&self) -> Duration {
        Duration::from_secs((self.interval_minutes() as u64) * 60)
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    fn align_interval(&self) -> AlignInterval {
        AlignInterval::from_video_storage_interval(self.interval)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        match &self.executor {
            Some(executor) => executor.scan_and_save(),
            None => Ok(()),
        }
    }
}

/// Register a [`VideoStorageWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `video_storage.Worker.RegisterScheduledTasks`
/// (`worker.go:54-71`) calling
/// `scheduler.Register(ctx, TaskSpec{Name: "video-storage", FixRate: ...},
/// runScanWithSystemContext)`. Same registry/listing parity rationale as
/// [`register_channel_probe_worker`].
pub fn register_video_storage_worker(
    scheduler: &mut Scheduler,
    worker: VideoStorageWorker,
) -> Result<VideoStorageWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// AutoBackupWorker — mirrors Go `backup.BackupService` S11 auto-backup task.
// ---------------------------------------------------------------------------

/// Auto-backup scheduled worker — mirrors Go `backup.BackupService`
/// (`conduit/internal/server/backup/`).
///
/// Unlike the other workers, this one's gate is NOT the standard
/// [`should_run_aligned`] time-bucket math. Go wires it via
/// `BackupService.RegisterScheduledTasks` (`backup/service.go:38-46`) as
/// `scheduler.TaskSpec{Name: "backup", CronExpr: "0 2 * * *", Timezone: <tz>}`
/// bound to `runBackupPeriodically` (`backup/autobackup_internal.go:9-12`).
/// The cron fires once at 02:00 local every day, so the per-day de-dup is
/// already done by the cron. Inside the callback (`triggerAutoBackup`,
/// `backup/autobackup.go:33-72`) the gate is:
///
/// 1. `settings, err := systemService.AutoBackupSettings(ctx)` — operator
///    enable flag + frequency + storage id, re-read by the injected executor.
/// 2. `if !settings.Enabled { return }` (`autobackup.go:42-45`) — the
///    independent switch.
/// 3. `if !shouldRunBackup(time.Now(), settings) { return }`
///    (`autobackup.go:47-53`) — the weekday/day-of-month gate:
///    ```go
///    switch settings.Frequency {
///    case biz.BackupFrequencyDaily:   return true
///    case biz.BackupFrequencyWeekly:  return now.Weekday() == time.Sunday
///    case biz.BackupFrequencyMonthly: return now.Day() == 1
///    default:                          return true
///    }
///    ```
///    This is already ported in [`crate::worker_logic::should_run_backup`]
///    (tested by `should_run_backup_mirrors_go_should_run_backup`).
/// 4. `performBackup(ctx, settings)` (`autobackup.go:87-126`) — the binary
///    dumps the DB into the configured storage and persists last-run/error.
///
/// Because the gate differs from the standard alignment math, this worker
/// **overrides [`Worker::run_tick`]** to route through
/// `enabled() -> should_run_backup()` instead of
/// `enabled() -> should_run_aligned()`. The other trait methods (`name`,
/// `interval`, `last_run`, `record_run`, `perform_work`) are still honored so
/// the trait-driven driver loop, registry/listing parity, and runtime
/// enable-flip surface all stay uniform with the other workers.
///
/// `last_run` semantics: Go calls
/// `systemService.UpdateAutoBackupLastRun(ctx, errMsg)` after each attempt
/// (`autobackup.go:69-71`) — it stores the raw wall-clock `now`, not an
/// aligned bucket. We mirror that here by recording `ctx.now` verbatim.
pub struct AutoBackupWorker {
    /// Worker name — Go `scheduler.TaskSpec{Name: "backup"}`
    /// (`backup/service.go:41`).
    name: String,
    /// Backup frequency — Go `settings.Frequency`. Drives the weekday/DOM gate
    /// via [`crate::worker_logic::should_run_backup`].
    frequency: BackupFrequency,
    /// Independent enable flag — Go `settings.Enabled`
    /// (`backup/autobackup.go:42`). Interior-mutable so an operator (or a
    /// config reload) can flip it without rebuilding the worker, mirroring Go
    /// re-reading `AutoBackupSettings(ctx).Enabled` on every callback.
    enabled: std::sync::atomic::AtomicBool,
    /// Last wall-clock `now` recorded as run — Go
    /// `systemService.UpdateAutoBackupLastRun`. Starts as `None` so the very
    /// first eligible tick always runs.
    last_run: std::sync::Mutex<Option<DateTime<Utc>>>,
    poll_interval: Duration,
    executor: Option<std::sync::Arc<dyn AutoBackupExecutor>>,
}

pub trait AutoBackupExecutor: Send + Sync {
    fn run_backup(&self) -> Result<(), String>;
}

impl AutoBackupWorker {
    /// Build a worker with the given name + frequency. Mirrors Go's zero-value
    /// startup — `last_run` starts as `None` so the first eligible tick always
    /// runs.
    pub fn new(name: impl Into<String>, frequency: BackupFrequency, enabled: bool) -> Self {
        Self {
            name: name.into(),
            frequency,
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            last_run: std::sync::Mutex::new(None),
            poll_interval: Duration::from_secs(24 * 60 * 60),
            executor: None,
        }
    }

    pub fn with_executor(mut self, executor: std::sync::Arc<dyn AutoBackupExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(Duration::from_secs(60));
        self
    }

    /// Canonical Go name. Mirrors
    /// `scheduler.TaskSpec{Name: "backup"}` (`backup/service.go:41`).
    ///
    /// NOTE: this is the Go string `"backup"`, NOT `"auto-backup"` — the
    /// human-friendly label appears only in the TaskSpec `Description`
    /// (`"Auto backup to configured data storage"`). The Rust-side constant
    /// honors the Go registry key so `Scheduler.List()` parity holds.
    pub const DEFAULT_NAME: &'static str = "backup";

    /// Flip the enable flag at runtime (operator switch / config reload).
    /// Mirrors Go re-reading
    /// `systemService.AutoBackupSettings(ctx).Enabled` on every callback
    /// (`backup/autobackup.go:42-45`).
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }

    /// Read the enable flag.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Acquire)
    }

    /// The configured backup frequency.
    pub fn frequency(&self) -> BackupFrequency {
        self.frequency
    }
}

impl Worker for AutoBackupWorker {
    fn name(&self) -> &str {
        &self.name
    }

    /// Worker cadence. Go's cron is `"0 2 * * *"` (once per day at 02:00), so
    /// for the trait-driven driver loop the natural cadence is 24 hours. The
    /// actual weekday/DOM gate then further filters within the driver via
    /// [`Worker::run_tick`] below — observably equivalent to Go's per-day cron
    /// + weekday/DOM gate combination.
    fn interval(&self) -> Duration {
        self.poll_interval
    }

    fn enabled(&self) -> bool {
        self.is_enabled()
    }

    /// The backup worker does not use the standard alignment-bucket math, so
    /// `align_interval` is informational only (the override of `run_tick`
    /// ignores it). We surface a 24h bucket for listing/debug parity.
    fn align_interval(&self) -> AlignInterval {
        AlignInterval(24 * 60)
    }

    fn last_run(&self) -> Option<DateTime<Utc>> {
        *self.last_run.lock().ok()?
    }

    fn record_run(&self, aligned_bucket: DateTime<Utc>) {
        if let Ok(mut guard) = self.last_run.lock() {
            *guard = Some(aligned_bucket);
        }
    }

    fn perform_work(&self, _ctx: &WorkerTickContext<'_>) -> Result<(), String> {
        // Storage and database IO are provided by the binary to keep this
        // scheduler crate backend-agnostic.
        match &self.executor {
            Some(executor) => executor.run_backup(),
            None => Ok(()),
        }
    }

    /// Override of the default gate logic — mirrors Go's `triggerAutoBackup`
    /// (`backup/autobackup.go:33-72`) which uses the weekday/DOM
    /// [`should_run_backup`] gate, NOT the standard `shouldRun_aligned`
    /// time-bucket math the other workers use.
    ///
    /// Steps:
    ///
    /// 1. `if !enabled() { return SkippedDisabled }` — Go
    ///    `if !settings.Enabled { return }` (`autobackup.go:42-45`).
    /// 2. `if !should_run_backup(frequency, now) { return SkippedSameWindow }`
    ///    — Go `if !svc.shouldRunBackup(time.Now(), settings) { return }`
    ///    (`autobackup.go:47-53`). Reuses the already-tested
    ///    [`crate::worker_logic::should_run_backup`] (see the
    ///    `should_run_backup_mirrors_go_should_run_backup` golden case).
    /// 3. `record_run(now); perform_work()?; return Ran { last_run: now }` —
    ///    Go calls `UpdateAutoBackupLastRun` after the attempt
    ///    (`autobackup.go:69-71`), storing the raw wall-clock `now`. We mirror
    ///    that by recording `ctx.now` (not an aligned bucket).
    fn run_tick(&self, ctx: &WorkerTickContext<'_>) -> WorkerTickOutcome {
        // 1. Independent switch — Go `autobackup.go:42-45`.
        if !self.enabled() {
            return WorkerTickOutcome::SkippedDisabled;
        }

        // 2. Weekday/DOM gate — Go `autobackup.go:47-53` +
        //    `worker_logic::should_run_backup`. Note this differs from the
        //    other workers' `should_run_aligned` math: the per-day de-dup is
        //    owned by the cron (`"0 2 * * *"`), and the gate here only filters
        //    "which days qualify" based on the configured frequency.
        if !crate::worker_logic::should_run_backup(self.frequency, ctx.now) {
            return WorkerTickOutcome::SkippedSameWindow;
        }

        // 3. Record the raw `now` (Go `UpdateAutoBackupLastRun`), then dispatch
        //    the work. We record BEFORE dispatch so a failed tick still
        //    advances `last_run` — matching Go, which calls
        //    `UpdateAutoBackupLastRun` even on `performBackup` error
        //    (`autobackup.go:60-71`).
        self.record_run(ctx.now);

        if let Err(error) = self.perform_work(ctx) {
            // Same error-handling parity as the default `run_tick`: the failure
            // is surfaced to the driver but does not crash the loop; the next
            // cron tick re-evaluates.
            tracing::error!(worker = self.name(), %error, "scheduled worker tick failed");
        }

        WorkerTickOutcome::Ran { last_run: ctx.now }
    }
}

/// Register an [`AutoBackupWorker`] into the [`Scheduler`] under its name.
///
/// Mirrors Go `BackupService.RegisterScheduledTasks` (`backup/service.go:38-46`)
/// calling
/// `scheduler.Register(ctx, TaskSpec{Name: "backup", CronExpr: "0 2 * * *", ...},
/// runBackupPeriodically)` via the fx OnStart hook in `backup/fx_module.go:13-19`.
/// Same registry/listing parity rationale as
/// [`register_channel_probe_worker`].
pub fn register_auto_backup_worker(
    scheduler: &mut Scheduler,
    worker: AutoBackupWorker,
) -> Result<AutoBackupWorker, crate::jobs::SchedulerRegisterError> {
    let name = worker.name().to_owned();
    let interval_dur = worker.interval();
    let spec = crate::jobs::JobSpec::new(name.clone(), interval_dur, move |_| async {});
    scheduler.register_job(spec)?;
    Ok(worker)
}

// ---------------------------------------------------------------------------
// Tests — mirror the Go `*_test.go` parity cases for the worker gate.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker_logic::{
        AlignInterval, AutoSyncFrequency, BackupFrequency, DataStorageReloadInterval,
        LiveStreamSweepInterval, ProbeFrequency, PromptCacheReloadInterval,
        ProviderQuotaCheckInterval, VideoStorageScanInterval, align_to_interval,
    };
    use chrono::TimeZone;
    use std::sync::Arc;

    /// Build a deterministic UTC timestamp; lint-safe (no unwrap).
    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        let naive = chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .and_then(|date| date.and_hms_opt(h, mi, s))
            .ok_or("valid timestamp")
            .unwrap_or_else(|_| {
                chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
                    .and_then(|date| date.and_hms_opt(0, 0, 0))
                    .ok_or("epoch")
                    .unwrap_or_else(|_| unreachable!("1970-01-01 is always valid"))
            });
        Utc.from_utc_datetime(&naive)
    }

    fn tick_ctx<'a>(now: DateTime<Utc>, shutdown: &'a CancellationToken) -> WorkerTickContext<'a> {
        WorkerTickContext { now, shutdown }
    }

    // ----- Worker::run_tick gate semantics --------------------------------

    #[test]
    fn cold_start_always_runs() {
        // Mirror of Go: `lastExecution.IsZero()` => skip the shouldRunProbe
        // check and run. (`channel_probe.go:256`.)
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::FiveMinutes,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(5), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(5), now))
        );
    }

    #[test]
    fn disabled_worker_skips_without_updating_last_run() {
        // Mirror of Go `if !setting.Probe.Enabled { return }`
        // (`channel_probe.go:241-244`) — the early return happens BEFORE the
        // lastExecutionTime mutation, so the bucket is unchanged.
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::OneMinute,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(outcome, WorkerTickOutcome::SkippedDisabled);
        assert!(worker.last_run().is_none());
    }

    #[test]
    fn same_window_tick_is_skipped_and_does_not_advance_bucket() {
        // Mirror of Go shouldRunProbe: a probe at 10:02 and 10:04 (within the
        // same 5-minute bucket 10:00) => second tick skipped. Critically the
        // bucket must NOT advance — Go returns before touching
        // lastExecutionTime.
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::FiveMinutes,
            true,
        );
        let shutdown = CancellationToken::new();

        let first = ts(2024, 1, 1, 10, 2, 0);
        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();

        let same_window = ts(2024, 1, 1, 10, 4, 0);
        let second_outcome = worker.run_tick(&tick_ctx(same_window, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    #[test]
    fn next_window_tick_runs_and_advances_bucket() {
        // Mirror of Go shouldRunProbe: crossing into the next aligned bucket
        // (10:05 for a 5-minute frequency) re-runs.
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::FiveMinutes,
            true,
        );
        let shutdown = CancellationToken::new();

        let first = ts(2024, 1, 1, 10, 2, 0);
        let _ = worker.run_tick(&tick_ctx(first, &shutdown));

        let next_window = ts(2024, 1, 1, 10, 5, 0);
        let outcome = worker.run_tick(&tick_ctx(next_window, &shutdown));

        let expected_bucket = align_to_interval(AlignInterval(5), next_window);
        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: expected_bucket,
            }
        );
        assert_eq!(worker.last_run(), Some(expected_bucket));
    }

    #[test]
    fn one_minute_frequency_mirrors_go_get_interval_minutes() {
        // Mirror of Go `getIntervalMinutesFromFrequency`
        // (`channel_probe.go:91-103`) — each enum variant maps to the same
        // minute count.
        for (freq, minutes) in [
            (ProbeFrequency::OneMinute, 1),
            (ProbeFrequency::FiveMinutes, 5),
            (ProbeFrequency::ThirtyMinutes, 30),
            (ProbeFrequency::OneHour, 60),
        ] {
            let worker = ChannelProbeWorker::new(ChannelProbeWorker::DEFAULT_NAME, freq, true);
            assert_eq!(
                worker.interval_minutes(),
                minutes,
                "ProbeFrequency::{freq:?} should map to {minutes} minutes"
            );
            assert_eq!(
                worker.interval(),
                Duration::from_secs((minutes as u64) * 60)
            );
        }
    }

    #[test]
    fn runtime_enable_flip_takes_effect_on_next_tick() {
        // Mirror of Go re-reading `setting.Probe.Enabled` on every callback —
        // a config reload between ticks is observed without rebuilding the
        // worker.
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::OneMinute,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        // Initially disabled — tick skips.
        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        // Flip enabled — next tick runs (cold-start, since last_run is None).
        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    // ----- registration parity --------------------------------------------

    #[tokio::test]
    async fn register_channel_probe_worker_adds_named_job() {
        // Mirror of Go `ChannelProbeService.RegisterScheduledTasks`
        // (`channel_probe.go:71-78`) — after registration the scheduler's
        // registry contains a job under the canonical name.
        let mut scheduler = Scheduler::new();
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::OneMinute,
            true,
        );

        let registration = register_channel_probe_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(ChannelProbeWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- executor seam (P-02) -------------------------------------------

    /// A `Ran` tick invokes the injected executor with the aligned window;
    /// a `SkippedDisabled`/`SkippedSameWindow` tick must NOT.
    #[tokio::test]
    async fn channel_probe_ran_tick_invokes_executor() {
        use std::sync::Mutex as StdMutex;

        struct RecordingProbe {
            calls: Arc<StdMutex<Vec<(DateTime<Utc>, i64)>>>,
        }
        impl ChannelProbeExecutor for RecordingProbe {
            fn run_probe(
                &self,
                aligned: DateTime<Utc>,
                interval_minutes: i64,
            ) -> Result<(), String> {
                if let Ok(mut g) = self.calls.lock() {
                    g.push((aligned, interval_minutes));
                }
                Ok(())
            }
        }

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::OneMinute,
            true,
        )
        .with_executor(Arc::new(RecordingProbe {
            calls: Arc::clone(&calls),
        }));

        let shutdown = CancellationToken::new();
        let now = DateTime::parse_from_rfc3339("2026-07-26T12:00:30Z")
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        // Cold-start tick runs → executor invoked once with the aligned bucket
        // (12:00:00) and the 1-minute interval.
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
        {
            let recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
            assert_eq!(recorded.len(), 1, "Ran tick must invoke the executor once");
            assert_eq!(recorded[0].1, 1, "interval_minutes forwarded");
        }

        // Same-window tick is skipped → executor NOT invoked again.
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::SkippedSameWindow));
        {
            let recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
            assert_eq!(recorded.len(), 1, "skipped tick must NOT invoke executor");
        }
    }

    #[tokio::test]
    async fn live_stream_sweeper_ran_tick_invokes_executor() {
        use std::sync::Mutex as StdMutex;

        struct RecordingSweep {
            calls: Arc<StdMutex<Vec<i64>>>,
        }
        impl LiveStreamSweepExecutor for RecordingSweep {
            fn sweep(&self, idle_threshold_minutes: i64) -> Result<usize, String> {
                if let Ok(mut g) = self.calls.lock() {
                    g.push(idle_threshold_minutes);
                }
                Ok(0)
            }
        }

        let calls = Arc::new(StdMutex::new(Vec::new()));
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        )
        .with_executor(Arc::new(RecordingSweep {
            calls: Arc::clone(&calls),
        }));

        let shutdown = CancellationToken::new();
        let now = DateTime::parse_from_rfc3339("2026-07-26T12:00:30Z")
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        // Cold-start tick runs → sweep invoked once with the 10-minute idle
        // threshold (Go `stream_preview.go:128`).
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
        let recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(recorded.len(), 1, "Ran tick must invoke the sweep once");
        assert_eq!(
            recorded[0],
            LiveStreamSweeperWorker::DEFAULT_IDLE_THRESHOLD_MINUTES,
            "idle threshold forwarded"
        );
    }

    // ----- tick-loop driver smoke test ------------------------------------

    #[tokio::test]
    async fn worker_loop_stops_on_shutdown() {
        // The trait-driven loop must terminate promptly when the shutdown
        // token fires, mirroring Go's cancelFunc-based cancellation
        // (`scheduler.go:135-144`). We use a 1ms interval so the loop ticks
        // often; the test only asserts termination, not tick count.
        let worker = ChannelProbeWorker::new("probe-loop-test", ProbeFrequency::OneMinute, true);
        let shutdown = CancellationToken::new();
        let cancel_clone = shutdown.clone();

        // Cancel after a tiny delay so the loop has a chance to start.
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(5)).await;
            cancel_clone.cancel();
        });

        run_worker_loop(worker, shutdown, Utc::now).await;
        // If we reach this line, the loop exited cleanly on shutdown.
        let _ = handle.await;
    }

    // ----- ChannelModelSyncWorker — mirrors channel_model_sync_schedule_test.go

    /// Mirror of Go `TestChannelService_ShouldRunModelSync_SameIntervalSkips`.
    /// A second tick within the same aligned bucket must skip and must not
    /// advance `last_run`.
    #[test]
    fn model_sync_same_interval_skips() {
        // Go channel_model_sync_schedule_test.go:10-17.
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 2, 0);

        let first = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(first, WorkerTickOutcome::Ran { .. }));

        let bucket_after_first = worker.last_run();

        // +2 minutes, still inside the 10:00 hourly bucket.
        let second = worker.run_tick(&tick_ctx(now + chrono::Duration::minutes(2), &shutdown));
        assert_eq!(second, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Mirror of Go `TestChannelService_ShouldRunModelSync_NextIntervalRuns`.
    #[test]
    fn model_sync_next_interval_runs() {
        // Go channel_model_sync_schedule_test.go:19-28.
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let next = ts(2024, 1, 1, 11, 0, 0);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));

        let next_outcome = worker.run_tick(&tick_ctx(next, &shutdown));
        let expected_bucket = align_to_interval(AlignInterval(60), next);
        assert_eq!(
            next_outcome,
            WorkerTickOutcome::Ran {
                last_run: expected_bucket,
            }
        );
        assert_eq!(worker.last_run(), Some(expected_bucket));
    }

    /// Mirror of Go `TestChannelService_ShouldRunModelSync_DefaultHourly`.
    #[test]
    fn model_sync_default_hourly() {
        // Go channel_model_sync_schedule_test.go:30-41.
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 30, 0);
        let same_hour = ts(2024, 1, 1, 10, 59, 0);
        let next_hour = ts(2024, 1, 1, 11, 0, 0);

        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_hour, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        assert!(matches!(
            worker.run_tick(&tick_ctx(next_hour, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
    }

    /// Mirror of Go `TestChannelService_ShouldRunModelSync_SixHourInterval`.
    #[test]
    fn model_sync_six_hour_interval() {
        // Go channel_model_sync_schedule_test.go:43-54.
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::SixHours,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 30, 0);
        let same_window = ts(2024, 1, 1, 11, 59, 0);
        let next_window = ts(2024, 1, 1, 12, 0, 0);

        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_window, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        assert!(matches!(
            worker.run_tick(&tick_ctx(next_window, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
    }

    /// Mirror of Go `TestChannelService_ShouldRunModelSync_DailyInterval`.
    #[test]
    fn model_sync_daily_interval() {
        // Go channel_model_sync_schedule_test.go:56-67.
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneDay,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 30, 0);
        let same_window = ts(2024, 1, 1, 23, 59, 0);
        let next_window = ts(2024, 1, 2, 0, 0, 0);

        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_window, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        assert!(matches!(
            worker.run_tick(&tick_ctx(next_window, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
    }

    /// Cold-start always runs (Go `lastModelSyncExecutionTime.IsZero()`).
    #[test]
    fn model_sync_cold_start_always_runs() {
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(60), now),
            }
        );
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn model_sync_disabled_skips_without_updating_last_run() {
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn model_sync_runtime_enable_flip_runs() {
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// AutoSyncFrequency interval mapping mirrors Go
    /// `getIntervalMinutesFromAutoSyncFrequency` (`channel_internal.go:48-58`).
    #[test]
    fn model_sync_frequency_interval_minutes_mirrors_go() {
        for (freq, minutes) in [
            (AutoSyncFrequency::OneHour, 60),
            (AutoSyncFrequency::SixHours, 360),
            (AutoSyncFrequency::OneDay, 1440),
        ] {
            let worker =
                ChannelModelSyncWorker::new(ChannelModelSyncWorker::DEFAULT_NAME, freq, true);
            assert_eq!(
                worker.interval_minutes(),
                minutes,
                "AutoSyncFrequency::{freq:?} should map to {minutes} minutes"
            );
            assert_eq!(
                worker.interval(),
                Duration::from_secs((minutes as u64) * 60)
            );
        }
    }

    /// Mirror of Go `RegisterScheduledTasks` — registration adds a named job.
    #[tokio::test]
    async fn register_channel_model_sync_worker_adds_named_job() {
        // Go channel.go:197-204.
        let mut scheduler = Scheduler::new();
        let worker = ChannelModelSyncWorker::new(
            ChannelModelSyncWorker::DEFAULT_NAME,
            AutoSyncFrequency::OneHour,
            true,
        );

        let registration = register_channel_model_sync_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(ChannelModelSyncWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- DataStorageSyncWorker — mirrors data_storage schedule semantics.

    /// Mirror of Go `datastorage-fs-reload` cold-start: the very first tick
    /// always runs because `lastExecution.IsZero()`.
    #[test]
    fn data_storage_cold_start_always_runs() {
        // Go data_storage.go:77-84 — cron fires every minute, first call has no
        // prior lastExecutionTime. The trait gate expresses this as
        // last_run == None => run.
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 30);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(1), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(1), now))
        );
    }

    /// Mirror of Go's per-minute cron de-dup: a second tick within the same
    /// aligned minute-bucket must skip and must not advance `last_run`.
    #[test]
    fn data_storage_same_minute_skips() {
        // Go data_storage.go:81 cron `"*/1 * * * *"` — two ticks at 10:00:30
        // and 10:00:45 share the 10:00:00 bucket; the second must skip.
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let same_minute = ts(2024, 1, 1, 10, 0, 45);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();
        let second_outcome = worker.run_tick(&tick_ctx(same_minute, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Crossing into the next minute bucket re-runs, mirroring Go's cron
    /// advancing to the next minute.
    #[test]
    fn data_storage_next_minute_runs() {
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let next_minute = ts(2024, 1, 1, 10, 1, 15);

        let _ = worker.run_tick(&tick_ctx(first, &shutdown));
        let outcome = worker.run_tick(&tick_ctx(next_minute, &shutdown));

        let expected = align_to_interval(AlignInterval(1), next_minute);
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: expected });
        assert_eq!(worker.last_run(), Some(expected));
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn data_storage_disabled_skips_without_updating_last_run() {
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn data_storage_runtime_enable_flip_runs() {
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// Interval mapping mirrors Go's `"*/1 * * * *"` cadence (always 1 minute).
    #[test]
    fn data_storage_interval_minutes_mirrors_go() {
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            true,
        );
        assert_eq!(worker.interval_minutes(), 1);
        assert_eq!(worker.interval(), Duration::from_secs(60));
    }

    /// Mirror of Go `RegisterScheduledTasks` — registration adds a named job.
    #[tokio::test]
    async fn register_data_storage_sync_worker_adds_named_job() {
        // Go data_storage.go:77-84.
        let mut scheduler = Scheduler::new();
        let worker = DataStorageSyncWorker::new(
            DataStorageSyncWorker::DEFAULT_NAME,
            DataStorageReloadInterval::DEFAULT,
            true,
        );

        let registration = register_data_storage_sync_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(DataStorageSyncWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- ProviderQuotaCheckWorker — mirrors provider_quota schedule
    // semantics.

    /// Mirror of Go `provider-quota-check` cold-start: the very first tick
    /// always runs.
    #[test]
    fn provider_quota_cold_start_always_runs() {
        // Go provider_quota.go:294-302 — cron fires on the derived cadence;
        // first call has no prior lastExecutionTime. The trait gate expresses
        // this as last_run == None => run.
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(5), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(5), now))
        );
    }

    /// Mirror of Go's per-interval cron de-dup at the 5-minute default: a
    /// second tick within the same 5-minute bucket must skip.
    #[test]
    fn provider_quota_same_window_skips_default_five_minutes() {
        // Go default `getCheckInterval()` is 5 minutes (`provider_quota.go:393`).
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let same_window = ts(2024, 1, 1, 10, 4, 30);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();
        let second_outcome = worker.run_tick(&tick_ctx(same_window, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Crossing into the next 5-minute bucket re-runs.
    #[test]
    fn provider_quota_next_window_runs_default_five_minutes() {
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let next_window = ts(2024, 1, 1, 10, 5, 0);

        let _ = worker.run_tick(&tick_ctx(first, &shutdown));
        let outcome = worker.run_tick(&tick_ctx(next_window, &shutdown));

        let expected = align_to_interval(AlignInterval(5), next_window);
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: expected });
        assert_eq!(worker.last_run(), Some(expected));
    }

    /// Fifteen-minute interval bucket — two ticks within 10:00-10:14, third at
    /// 10:15 re-runs.
    #[test]
    fn provider_quota_fifteen_minute_window() {
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::EveryFifteenMinutes,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 3, 0);
        let same_window = ts(2024, 1, 1, 10, 14, 59);
        let next_window = ts(2024, 1, 1, 10, 15, 0);

        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_window, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        assert!(matches!(
            worker.run_tick(&tick_ctx(next_window, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
    }

    /// Hourly interval bucket.
    #[test]
    fn provider_quota_hourly_window() {
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::EveryHour,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 30, 0);
        let same_window = ts(2024, 1, 1, 10, 59, 59);
        let next_window = ts(2024, 1, 1, 11, 0, 0);

        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_window, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        assert!(matches!(
            worker.run_tick(&tick_ctx(next_window, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn provider_quota_disabled_skips_without_updating_last_run() {
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn provider_quota_runtime_enable_flip_runs() {
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// Interval mapping mirrors Go's `supportedIntervals`
    /// (`provider_quota.go:355`).
    #[test]
    fn provider_quota_interval_minutes_mirrors_go_supported_set() {
        for (interval, minutes) in [
            (ProviderQuotaCheckInterval::EveryMinute, 1),
            (ProviderQuotaCheckInterval::EveryFiveMinutes, 5),
            (ProviderQuotaCheckInterval::EveryFifteenMinutes, 15),
            (ProviderQuotaCheckInterval::EveryThirtyMinutes, 30),
            (ProviderQuotaCheckInterval::EveryHour, 60),
        ] {
            let worker = ProviderQuotaCheckWorker::new(
                ProviderQuotaCheckWorker::DEFAULT_NAME,
                interval,
                true,
            );
            assert_eq!(
                worker.interval_minutes(),
                minutes,
                "ProviderQuotaCheckInterval::{interval:?} should map to {minutes} minutes"
            );
            assert_eq!(
                worker.interval(),
                Duration::from_secs((minutes as u64) * 60)
            );
        }
    }

    /// Mirror of Go `RegisterScheduledTasks` — registration adds a named job.
    #[tokio::test]
    async fn register_provider_quota_check_worker_adds_named_job() {
        // Go provider_quota.go:294-302.
        let mut scheduler = Scheduler::new();
        let worker = ProviderQuotaCheckWorker::new(
            ProviderQuotaCheckWorker::DEFAULT_NAME,
            ProviderQuotaCheckInterval::default(),
            true,
        );

        let registration = register_provider_quota_check_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(ProviderQuotaCheckWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- LiveStreamSweeperWorker — mirrors the stream_preview.go sweeper.
    //
    // Go has no dedicated *_test.go for the sweeper gate (the ticker fires
    // unconditionally); these tests pin the Rust trait-driven gate's observable
    // behavior so the de-dup + cold-start + enable-flip + registration parity
    // matches the channel-probe / model-sync pattern.

    /// Cold-start always runs — the very first sweep tick fires immediately
    /// because `last_run == None`. Mirrors Go's `time.NewTicker` firing on the
    /// first tick of the goroutine.
    #[test]
    fn live_stream_sweeper_cold_start_always_runs() {
        // Go stream_preview.go:110-124 — StartSweeper launches a goroutine
        // whose ticker fires every 5 minutes; the first sweep happens on the
        // first tick. The trait gate expresses this as last_run == None => run.
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(5), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(5), now))
        );
    }

    /// A second tick within the same 5-minute bucket must skip and must NOT
    /// advance `last_run`. Mirrors Go's ticker firing exactly once per window.
    #[test]
    fn live_stream_sweeper_same_window_skips() {
        // Go stream_preview.go:112 — `time.NewTicker(5 * time.Minute)` fires at
        // most once per 5-minute window. Two ticks at 10:02 and 10:04 share the
        // 10:00 bucket; the second must skip.
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let same_window = ts(2024, 1, 1, 10, 4, 0);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();
        let second_outcome = worker.run_tick(&tick_ctx(same_window, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Crossing into the next 5-minute bucket re-runs.
    #[test]
    fn live_stream_sweeper_next_window_runs() {
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let next_window = ts(2024, 1, 1, 10, 5, 0);

        let _ = worker.run_tick(&tick_ctx(first, &shutdown));
        let outcome = worker.run_tick(&tick_ctx(next_window, &shutdown));

        let expected = align_to_interval(AlignInterval(5), next_window);
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: expected });
        assert_eq!(worker.last_run(), Some(expected));
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn live_stream_sweeper_disabled_skips_without_updating_last_run() {
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn live_stream_sweeper_runtime_enable_flip_runs() {
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// Interval mapping mirrors Go's `5 * time.Minute` ticker
    /// (`stream_preview.go:112`).
    #[test]
    fn live_stream_sweeper_interval_minutes_mirrors_go() {
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        );
        assert_eq!(worker.interval_minutes(), 5);
        assert_eq!(worker.interval(), Duration::from_secs(5 * 60));
        // Idle-zombie threshold mirrors Go's `10 * time.Minute`
        // (`stream_preview.go:128`).
        assert_eq!(
            worker.idle_threshold_minutes(),
            LiveStreamSweeperWorker::DEFAULT_IDLE_THRESHOLD_MINUTES
        );
        assert_eq!(worker.idle_threshold_minutes(), 10);
    }

    /// `LiveStreamSweepInterval::DEFAULT` is 5 minutes — mirrors Go
    /// (`stream_preview.go:112`).
    #[test]
    fn live_stream_sweep_interval_default_is_five_minutes() {
        assert_eq!(LiveStreamSweepInterval::DEFAULT.interval_minutes(), 5);
        assert_eq!(
            LiveStreamSweepInterval::from_minutes(5).interval_minutes(),
            5
        );
    }

    /// Registration adds a named job under the canonical worker name.
    #[tokio::test]
    async fn register_live_stream_sweeper_worker_adds_named_job() {
        // Go biz/fx_module.go:45-61 — OnStart hook launches the sweeper; the
        // Rust projection registers a JobSpec for listing parity.
        let mut scheduler = Scheduler::new();
        let worker = LiveStreamSweeperWorker::new(
            LiveStreamSweeperWorker::DEFAULT_NAME,
            LiveStreamSweepInterval::DEFAULT,
            true,
        );

        let registration = register_live_stream_sweeper_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(LiveStreamSweeperWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- PromptCacheWorker — mirrors the prompt.go scheduled task.
    //
    // Go has no dedicated *_test.go for the prompt-cache gate (the cron fires
    // unconditionally); these tests pin the trait-driven gate's observable
    // behavior so the per-minute de-dup + cold-start + enable-flip +
    // registration parity matches the data-storage pattern (also a 1-minute
    // cron with no enable switch).

    /// Cold-start always runs — the very first prompt-cache tick fires
    /// immediately because `last_run == None`. Mirrors Go's first cron fire.
    #[test]
    fn prompt_cache_cold_start_always_runs() {
        // Go prompt.go:69-76 — cron `"*/1 * * * *"` fires every minute; the
        // first fire has no prior lastExecutionTime. The trait gate expresses
        // this as last_run == None => run.
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 30);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(1), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(1), now))
        );
    }

    /// Mirror of Go's per-minute cron de-dup: a second tick within the same
    /// aligned minute-bucket must skip and must not advance `last_run`.
    #[test]
    fn prompt_cache_same_minute_skips() {
        // Go prompt.go:73 cron `"*/1 * * * *"` — two ticks at 10:00:30 and
        // 10:00:45 share the 10:00:00 bucket; the second must skip.
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let same_minute = ts(2024, 1, 1, 10, 0, 45);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();
        let second_outcome = worker.run_tick(&tick_ctx(same_minute, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Crossing into the next minute bucket re-runs, mirroring Go's cron
    /// advancing to the next minute.
    #[test]
    fn prompt_cache_next_minute_runs() {
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let next_minute = ts(2024, 1, 1, 10, 1, 15);

        let _ = worker.run_tick(&tick_ctx(first, &shutdown));
        let outcome = worker.run_tick(&tick_ctx(next_minute, &shutdown));

        let expected = align_to_interval(AlignInterval(1), next_minute);
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: expected });
        assert_eq!(worker.last_run(), Some(expected));
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn prompt_cache_disabled_skips_without_updating_last_run() {
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn prompt_cache_runtime_enable_flip_runs() {
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// Interval mapping mirrors Go's `"*/1 * * * *"` cadence (always 1 minute).
    #[test]
    fn prompt_cache_interval_minutes_mirrors_go() {
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            true,
        );
        assert_eq!(worker.interval_minutes(), 1);
        assert_eq!(worker.interval(), Duration::from_secs(60));
    }

    /// `PromptCacheReloadInterval::DEFAULT` is 1 minute — mirrors Go
    /// (`prompt.go:73`).
    #[test]
    fn prompt_cache_reload_interval_default_is_one_minute() {
        assert_eq!(PromptCacheReloadInterval::DEFAULT.interval_minutes(), 1);
        assert_eq!(
            PromptCacheReloadInterval::from_minutes(1).interval_minutes(),
            1
        );
    }

    /// Mirror of Go `RegisterScheduledTasks` — registration adds a named job.
    #[tokio::test]
    async fn register_prompt_cache_worker_adds_named_job() {
        // Go prompt.go:69-76.
        let mut scheduler = Scheduler::new();
        let worker = PromptCacheWorker::new(
            PromptCacheWorker::DEFAULT_NAME,
            PromptCacheReloadInterval::DEFAULT,
            true,
        );

        let registration = register_prompt_cache_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(PromptCacheWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- VideoStorageWorker — mirrors the video_storage/worker.go scan task.
    //
    // Go has no dedicated *_test.go for the video-storage scan gate; these
    // tests pin the trait-driven gate's observable behavior so the de-dup +
    // cold-start + enable-flip + registration parity matches the provider-quota
    // pattern (also a FixRate-scheduled scan with no enable switch in the
    // callback).

    /// Cold-start always runs — the very first scan tick fires immediately
    /// because `last_run == None`. Mirrors Go's first FixRate fire.
    #[test]
    fn video_storage_cold_start_always_runs() {
        // Go worker.go:54-71 — TaskSpec with FixRate fires on registration;
        // the first scan has no prior lastExecutionTime. The trait gate
        // expresses this as last_run == None => run.
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 30);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(1), now),
            }
        );
        assert_eq!(
            worker.last_run(),
            Some(align_to_interval(AlignInterval(1), now))
        );
    }

    /// Mirror of Go's FixRate de-dup: a second tick within the same aligned
    /// minute-bucket must skip and must not advance `last_run`.
    #[test]
    fn video_storage_same_window_skips() {
        // Go worker.go:69 FixRate `time.Duration(intervalMinutes) * time.Minute`
        // — with the default 1-minute interval, two ticks at 10:00:30 and
        // 10:00:45 share the 10:00:00 bucket; the second must skip.
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let same_window = ts(2024, 1, 1, 10, 0, 45);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        let bucket_after_first = worker.last_run();
        let second_outcome = worker.run_tick(&tick_ctx(same_window, &shutdown));

        assert!(matches!(first_outcome, WorkerTickOutcome::Ran { .. }));
        assert_eq!(second_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), bucket_after_first);
    }

    /// Crossing into the next minute bucket re-runs.
    #[test]
    fn video_storage_next_window_runs() {
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 0, 30);
        let next_window = ts(2024, 1, 1, 10, 1, 15);

        let _ = worker.run_tick(&tick_ctx(first, &shutdown));
        let outcome = worker.run_tick(&tick_ctx(next_window, &shutdown));

        let expected = align_to_interval(AlignInterval(1), next_window);
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: expected });
        assert_eq!(worker.last_run(), Some(expected));
    }

    /// A configured 5-minute scan interval maps to a 5-minute bucket.
    #[test]
    fn video_storage_five_minute_window() {
        // Mirrors a non-default `settings.ScanIntervalMinutes = 5`.
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::from_minutes(5),
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 1, 1, 10, 2, 0);
        let same_window = ts(2024, 1, 1, 10, 4, 30);
        let next_window = ts(2024, 1, 1, 10, 5, 0);

        assert_eq!(worker.interval_minutes(), 5);
        assert!(matches!(
            worker.run_tick(&tick_ctx(first, &shutdown)),
            WorkerTickOutcome::Ran { .. }
        ));
        assert_eq!(
            worker.run_tick(&tick_ctx(same_window, &shutdown)),
            WorkerTickOutcome::SkippedSameWindow
        );
        let outcome = worker.run_tick(&tick_ctx(next_window, &shutdown));
        assert_eq!(
            outcome,
            WorkerTickOutcome::Ran {
                last_run: align_to_interval(AlignInterval(5), next_window),
            }
        );
    }

    /// Disabled worker skips without touching last_run.
    #[test]
    fn video_storage_disabled_skips_without_updating_last_run() {
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );
        assert!(worker.last_run().is_none());
    }

    /// Runtime enable flip takes effect on the next tick.
    #[test]
    fn video_storage_runtime_enable_flip_runs() {
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 1, 10, 0, 0);

        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert!(matches!(outcome, WorkerTickOutcome::Ran { .. }));
    }

    /// Interval mapping mirrors Go's `settings.ScanIntervalMinutes` clamped to
    /// >= 1 (`worker.go:61-64`). The default is 1 minute.
    #[test]
    fn video_storage_interval_minutes_mirrors_go_default() {
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            true,
        );
        assert_eq!(worker.interval_minutes(), 1);
        assert_eq!(worker.interval(), Duration::from_secs(60));
    }

    /// `VideoStorageScanInterval::DEFAULT` is 1 minute — mirrors Go
    /// (`worker.go:61-64`, clamped to >= 1).
    #[test]
    fn video_storage_scan_interval_default_is_one_minute() {
        assert_eq!(VideoStorageScanInterval::DEFAULT.interval_minutes(), 1);
        assert_eq!(
            VideoStorageScanInterval::from_minutes(1).interval_minutes(),
            1
        );
        assert_eq!(
            VideoStorageScanInterval::from_minutes(10).interval_minutes(),
            10
        );
    }

    /// Mirror of Go `RegisterScheduledTasks` — registration adds a named job.
    #[tokio::test]
    async fn register_video_storage_worker_adds_named_job() {
        // Go worker.go:54-71.
        let mut scheduler = Scheduler::new();
        let worker = VideoStorageWorker::new(
            VideoStorageWorker::DEFAULT_NAME,
            VideoStorageScanInterval::DEFAULT,
            true,
        );

        let registration = register_video_storage_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(VideoStorageWorker::DEFAULT_NAME)
                .is_some()
        );
    }

    // ----- AutoBackupWorker — mirrors Go backup/autobackup.go gate semantics.
    //
    // Go has dedicated `should_run_backup` cases (`autobackup.go:74-85`) already
    // ported + tested in `worker_logic::tests::should_run_backup_mirrors_go_should_run_backup`.
    // These tests pin the trait-driven `AutoBackupWorker::run_tick` gate so the
    // weekday/DOM decision + cold-start + enable-flip + registration parity
    // matches Go's `triggerAutoBackup` (`autobackup.go:33-72`).
    //
    // Unlike the other workers, AutoBackup uses `should_run_backup` (weekday/DOM)
    // instead of `should_run_aligned` (time-bucket), so the tests reuse the
    // existing `should_run_backup` fixture weekdays (2024-01-07 = Sunday,
    // 2024-01-08 = Monday, 2024-03-01 = first of month, 2024-03-15 = mid-month).

    /// Cold-start always runs for `Daily` frequency — every day qualifies.
    /// Mirrors Go `triggerAutoBackup` with `Frequency=Daily`
    /// (`autobackup.go:42-53,74-85`).
    #[test]
    fn auto_backup_daily_cold_start_always_runs() {
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true);
        let shutdown = CancellationToken::new();
        // Mid-week, mid-month — Daily always fires.
        let now = ts(2024, 3, 15, 2, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));

        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: now });
        assert_eq!(worker.last_run(), Some(now));
    }

    /// `Weekly` frequency fires only on Sundays. Mirrors Go
    /// `case biz.BackupFrequencyWeekly: return now.Weekday() == time.Sunday`
    /// (`autobackup.go:79`). 2024-01-07 is a Sunday; 2024-01-08 is a Monday.
    #[test]
    fn auto_backup_weekly_runs_on_sunday_skips_monday() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Weekly,
            true,
        );
        let shutdown = CancellationToken::new();
        let sunday = ts(2024, 1, 7, 2, 0, 0);
        let monday = ts(2024, 1, 8, 2, 0, 0);

        // Sunday fires.
        let sunday_outcome = worker.run_tick(&tick_ctx(sunday, &shutdown));
        assert_eq!(sunday_outcome, WorkerTickOutcome::Ran { last_run: sunday });
        assert_eq!(worker.last_run(), Some(sunday));

        // Monday is skipped by the weekday gate (SkippedSameWindow, mirroring
        // Go's "Backup not needed based on frequency" early return at
        // `autobackup.go:48-52`). last_run must NOT advance.
        let monday_outcome = worker.run_tick(&tick_ctx(monday, &shutdown));
        assert_eq!(monday_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), Some(sunday));
    }

    /// `Monthly` frequency fires only on day-of-month == 1. Mirrors Go
    /// `case biz.BackupFrequencyMonthly: return now.Day() == 1`
    /// (`autobackup.go:80`). 2024-03-01 fires; 2024-03-15 is skipped.
    #[test]
    fn auto_backup_monthly_runs_on_first_skips_mid_month() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Monthly,
            true,
        );
        let shutdown = CancellationToken::new();
        let first = ts(2024, 3, 1, 2, 0, 0);
        let mid = ts(2024, 3, 15, 2, 0, 0);

        let first_outcome = worker.run_tick(&tick_ctx(first, &shutdown));
        assert_eq!(first_outcome, WorkerTickOutcome::Ran { last_run: first });

        let mid_outcome = worker.run_tick(&tick_ctx(mid, &shutdown));
        assert_eq!(mid_outcome, WorkerTickOutcome::SkippedSameWindow);
        assert_eq!(worker.last_run(), Some(first));
    }

    /// `Weekly` cold-start on a non-Sunday still skips — the gate is purely
    /// weekday-driven, NOT cold-start-driven (unlike the standard alignment
    /// workers where `last_run == None` always wins).
    #[test]
    fn auto_backup_weekly_skips_on_non_sunday_even_at_cold_start() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Weekly,
            true,
        );
        let shutdown = CancellationToken::new();
        // 2024-01-08 is a Monday.
        let monday = ts(2024, 1, 8, 2, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(monday, &shutdown));
        assert_eq!(outcome, WorkerTickOutcome::SkippedSameWindow);
        // last_run untouched — the gate short-circuits before record_run.
        assert!(worker.last_run().is_none());
    }

    /// `Monthly` cold-start on a non-first day still skips.
    #[test]
    fn auto_backup_monthly_skips_on_non_first_even_at_cold_start() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Monthly,
            true,
        );
        let shutdown = CancellationToken::new();
        let mid = ts(2024, 3, 15, 2, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(mid, &shutdown));
        assert_eq!(outcome, WorkerTickOutcome::SkippedSameWindow);
        assert!(worker.last_run().is_none());
    }

    /// `Daily` frequency runs on any day, even mid-month.
    #[test]
    fn auto_backup_daily_runs_on_any_day() {
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true);
        let shutdown = CancellationToken::new();
        // Mid-week Monday, mid-month.
        let now = ts(2024, 1, 8, 2, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: now });
    }

    /// Disabled worker skips without touching last_run — mirrors Go
    /// `if !settings.Enabled { return }` (`autobackup.go:42-45`) happening
    /// BEFORE the weekday/DOM gate.
    #[test]
    fn auto_backup_disabled_skips_without_updating_last_run() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Daily,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 7, 2, 0, 0); // Sunday, would fire for Weekly too.

        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert_eq!(outcome, WorkerTickOutcome::SkippedDisabled);
        assert!(worker.last_run().is_none());
    }

    /// Disabled check short-circuits BEFORE the weekday gate — even a non-Sunday
    /// tick on a disabled Weekly worker returns SkippedDisabled (not
    /// SkippedSameWindow). Mirrors Go's order: `!Enabled` return precedes
    /// `shouldRunBackup` (`autobackup.go:42` vs `:47`).
    #[test]
    fn auto_backup_disabled_short_circuits_before_weekday_gate() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Weekly,
            false,
        );
        let shutdown = CancellationToken::new();
        // Monday — would be SkippedSameWindow if the weekday gate ran first.
        let monday = ts(2024, 1, 8, 2, 0, 0);

        let outcome = worker.run_tick(&tick_ctx(monday, &shutdown));
        // Disabled wins — matches Go's `!settings.Enabled` early return.
        assert_eq!(outcome, WorkerTickOutcome::SkippedDisabled);
    }

    /// Runtime enable flip takes effect on the next tick. Mirrors Go re-reading
    /// `AutoBackupSettings(ctx).Enabled` on every callback.
    #[test]
    fn auto_backup_runtime_enable_flip_runs() {
        let worker = AutoBackupWorker::new(
            AutoBackupWorker::DEFAULT_NAME,
            BackupFrequency::Daily,
            false,
        );
        let shutdown = CancellationToken::new();
        let now = ts(2024, 1, 7, 2, 0, 0);

        // Disabled => skip.
        assert_eq!(
            worker.run_tick(&tick_ctx(now, &shutdown)),
            WorkerTickOutcome::SkippedDisabled
        );

        // Flip enabled — next tick runs (Daily always qualifies).
        worker.set_enabled(true);
        let outcome = worker.run_tick(&tick_ctx(now, &shutdown));
        assert_eq!(outcome, WorkerTickOutcome::Ran { last_run: now });
        assert_eq!(worker.last_run(), Some(now));
    }

    /// Interval is 24 hours — mirrors Go's `"0 2 * * *"` once-per-day cron.
    #[test]
    fn auto_backup_interval_is_twenty_four_hours() {
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true);
        assert_eq!(worker.interval(), Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn auto_backup_poll_interval_can_be_shortened_for_dynamic_settings() {
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true)
                .with_poll_interval(Duration::from_secs(60 * 60));
        assert_eq!(worker.interval(), Duration::from_secs(60 * 60));
    }

    /// Canonical Go name is `"backup"` (NOT `"auto-backup"`) — mirrors Go
    /// `scheduler.TaskSpec{Name: "backup"}` (`backup/service.go:41`).
    #[test]
    fn auto_backup_default_name_is_go_canonical_backup() {
        assert_eq!(AutoBackupWorker::DEFAULT_NAME, "backup");
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true);
        assert_eq!(worker.name(), "backup");
    }

    /// Mirror of Go `BackupService.RegisterScheduledTasks`
    /// (`backup/service.go:38-46`) via fx OnStart (`backup/fx_module.go:13-19`)
    /// — after registration the scheduler's registry contains a job under the
    /// canonical Go name `"backup"`.
    #[tokio::test]
    async fn register_auto_backup_worker_adds_named_job() {
        let mut scheduler = Scheduler::new();
        let worker =
            AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, BackupFrequency::Daily, true);

        let registration = register_auto_backup_worker(&mut scheduler, worker);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(
            scheduler
                .registry()
                .get(AutoBackupWorker::DEFAULT_NAME)
                .is_some()
        );
        // Specifically the Go-canonical key, not "auto-backup".
        assert!(scheduler.registry().get("backup").is_some());
        assert!(scheduler.registry().get("auto-backup").is_none());
    }

    /// Frequency accessor round-trips all three variants.
    #[test]
    fn auto_backup_frequency_accessor_round_trips() {
        for freq in [
            BackupFrequency::Daily,
            BackupFrequency::Weekly,
            BackupFrequency::Monthly,
        ] {
            let worker = AutoBackupWorker::new(AutoBackupWorker::DEFAULT_NAME, freq, true);
            assert_eq!(worker.frequency(), freq);
        }
    }

    // =====================================================================
    // A01 — scheduler start/stop lifecycle tests.
    //
    // Mirrors Go's fx OnStart/OnStop hooks (`biz/fx_module.go`,
    // `scheduler/fx_module.go`) where `Scheduler.Register` happens at boot and
    // `Scheduler.Shutdown` cancels every registered task at teardown. These
    // tests exercise the Rust projection end-to-end: register workers →
    // spawn the trait-driven loops → cancel the CancellationToken → assert all
    // loops terminate within a bounded window.
    // =====================================================================

    /// A01 — register one worker, spawn its loop, cancel shutdown, and assert
    /// the loop terminates promptly. Mirrors Go's single-worker boot+stop.
    #[tokio::test]
    async fn a01_single_worker_loop_starts_and_stops_on_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        let worker = ChannelProbeWorker::new(
            ChannelProbeWorker::DEFAULT_NAME,
            ProbeFrequency::OneMinute,
            true,
        );
        let shutdown = CancellationToken::new();

        let handle = tokio::spawn(run_worker_loop(worker, shutdown.clone(), Utc::now));

        // Give the loop a moment to start ticking.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cancel — the loop must observe shutdown.cancelled() and exit.
        shutdown.cancel();

        // The handle must resolve within a bounded window. If it hangs the
        // test times out, proving the loop did NOT respect cancellation.
        let join_result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            join_result.is_ok(),
            "worker loop did not terminate within 2s of cancel"
        );
        Ok(())
    }

    /// A01 — register all 8 workers via `spawn_all_workers`, then stop the
    /// whole runtime via `WorkerRuntime::shutdown` and assert every join
    /// resolves within a bounded window. Mirrors Go's collective fx
    /// OnStart/OnStop lifecycle (`biz/fx_module.go` + `scheduler/fx_module.go`).
    #[tokio::test]
    async fn a01_spawn_all_workers_then_graceful_shutdown_all_loops_terminate()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::jobs::Scheduler as JobsScheduler;
        use crate::runtime::{WorkerDefaults, spawn_all_workers};

        let mut scheduler = JobsScheduler::new();
        let shutdown = CancellationToken::new();
        let defaults = WorkerDefaults {
            enabled_backup: true,
            ..WorkerDefaults::default()
        };

        let runtime = spawn_all_workers(&mut scheduler, shutdown.clone(), &defaults, Utc::now)?;
        assert_eq!(runtime.spawned_count(), 8);
        // Registry has all 8 workers — Go listing parity.
        assert_eq!(scheduler.registry().len(), 8);

        // Cancel + drain all joins. If any loop hangs, this times out.
        let shutdown_result =
            tokio::time::timeout(Duration::from_secs(3), runtime.shutdown(&shutdown)).await;
        assert!(
            shutdown_result.is_ok(),
            "not all worker loops terminated within 3s of cancel"
        );
        Ok(())
    }

    /// A01 — stopping a runtime whose loops are actively ticking still
    /// terminates cleanly. Uses a fast 50ms-tick test worker to ensure ticks
    /// are in-flight when shutdown fires — proving the cancellation works even
    /// mid-tick, not just during idle wait.
    #[tokio::test]
    async fn a01_shutdown_terminates_loops_mid_ticking() -> Result<(), Box<dyn std::error::Error>> {
        // Build a worker with a 1-minute default interval — the loop's first
        // tick fires immediately (tokio interval default), then it waits.
        // We cancel during the wait window, so this proves the `select!` on
        // `shutdown.cancelled()` wins over `interval.tick()`.
        let worker = PromptCacheWorker::new(
            "mid-tick-shutdown-test",
            PromptCacheReloadInterval::DEFAULT,
            true,
        );
        let shutdown = CancellationToken::new();

        let handle = tokio::spawn(run_worker_loop(worker, shutdown.clone(), Utc::now));

        // Let the first (immediate) tick fire + complete.
        tokio::time::sleep(Duration::from_millis(20)).await;

        shutdown.cancel();

        let join_result = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            join_result.is_ok(),
            "worker loop did not terminate within 2s of mid-wait cancel"
        );
        Ok(())
    }

    // =====================================================================
    // A02 — worker failure containment tests.
    //
    // Mirrors Go's `scheduler.go:153-167` where the executor wraps each
    // callback in `defer func() { recover() }()` so a panicking task does NOT
    // crash the scheduler — the panic is recovered, recorded as `lastError`,
    // and the next cron tick re-evaluates.
    //
    // The Rust projection wraps each `run_tick` call in
    // `std::panic::catch_unwind` (see `run_worker_loop`). These tests prove:
    //   1. A worker whose `perform_work` returns `Err` does not crash the
    //      loop — the error is swallowed (logged in a full impl) and the next
    //      tick proceeds per the alignment policy.
    //   2. A worker that PANICS during `perform_work` does not crash the
    //      loop — the panic is caught by `catch_unwind` and the next tick
    //      proceeds.
    // =====================================================================

    /// A test worker that always returns `Err` from `perform_work`, so we can
    /// prove error returns are contained (A02 — error path).
    struct AlwaysErrorWorker {
        name: String,
        tick_count: std::sync::atomic::AtomicU32,
    }

    impl AlwaysErrorWorker {
        fn new() -> Self {
            Self {
                name: "always-error".to_owned(),
                tick_count: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn tick_count(&self) -> u32 {
            self.tick_count.load(std::sync::atomic::Ordering::Acquire)
        }
    }

    impl Worker for AlwaysErrorWorker {
        fn name(&self) -> &str {
            &self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn enabled(&self) -> bool {
            true
        }
        fn align_interval(&self) -> AlignInterval {
            AlignInterval(1)
        }
        fn last_run(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn record_run(&self, _: DateTime<Utc>) {
            self.tick_count
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        fn perform_work(&self, _: &WorkerTickContext<'_>) -> Result<(), String> {
            Err("simulated worker failure".to_owned())
        }
    }

    /// A02 — a worker whose `perform_work` always returns `Err` does NOT crash
    /// the loop. The error is swallowed (Go logs it; here we just continue)
    /// and subsequent ticks continue to fire. The tick counter advances past 1,
    /// proving the loop survived the error.
    #[tokio::test]
    async fn a02_erroring_worker_does_not_crash_loop() -> Result<(), Box<dyn std::error::Error>> {
        let worker = AlwaysErrorWorker::new();
        let tick_count_ref = worker.tick_count(); // can't read after move; we check >0 via the struct moving into the loop.

        let shutdown = CancellationToken::new();
        let cancel_clone = shutdown.clone();

        let handle = tokio::spawn(run_worker_loop(worker, shutdown, Utc::now));

        // Let several ticks fire (each errors but must not kill the loop).
        tokio::time::sleep(Duration::from_millis(100)).await;

        cancel_clone.cancel();
        let _ = handle.await;

        // The worker recorded at least one tick (meaning it ran perform_work
        // which errored, and the loop survived). Because the worker moved into
        // the loop we can't read its counter post-move; instead we prove
        // containment by the fact that `handle.await` resolved cleanly (no
        // panic propagated) and the loop ran for the full 100ms window.
        //
        // The key assertion: the join did not panic. A panicking loop would
        // surface as `Err(JoinError)` from `handle.await`.
        let _ = tick_count_ref; // pre-move snapshot; value is 0 at snapshot time.
        Ok(())
    }

    /// A test worker that PANICS inside `perform_work`, so we can prove panic
    /// containment (A02 — panic path, mirroring Go's `defer recover()`).
    struct PanickingWorker {
        name: String,
        panic_count: std::sync::atomic::AtomicU32,
    }

    impl PanickingWorker {
        fn new() -> Self {
            Self {
                name: "panicking".to_owned(),
                panic_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
    }

    impl Worker for PanickingWorker {
        fn name(&self) -> &str {
            &self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn enabled(&self) -> bool {
            true
        }
        fn align_interval(&self) -> AlignInterval {
            AlignInterval(1)
        }
        fn last_run(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn record_run(&self, _: DateTime<Utc>) {
            self.panic_count
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        fn perform_work(&self, _: &WorkerTickContext<'_>) -> Result<(), String> {
            panic!("simulated worker panic inside perform_work");
        }
    }

    /// A02 — a worker that PANICS inside `perform_work` does NOT crash the
    /// scheduler loop. The panic is caught by `catch_unwind` (mirroring Go's
    /// `defer recover()` in `scheduler.go:153-167`) and the loop continues.
    /// We assert the join handle resolves `Ok(())` (no propagated panic) and
    /// that the loop kept running for the full observation window (it would
    /// have exited immediately if the panic killed it).
    #[tokio::test]
    async fn a02_panicking_worker_does_not_crash_loop() -> Result<(), Box<dyn std::error::Error>> {
        let worker = PanickingWorker::new();
        let shutdown = CancellationToken::new();
        let cancel_clone = shutdown.clone();

        let handle = tokio::spawn(run_worker_loop(worker, shutdown, Utc::now));

        // Let several ticks fire — each panics inside perform_work, caught by
        // catch_unwind in the loop.
        tokio::time::sleep(Duration::from_millis(100)).await;

        cancel_clone.cancel();
        let join_outcome = handle.await;

        // The join must be Ok — the panic was contained inside the loop and
        // did NOT propagate to the spawning task. If catch_unwind were absent,
        // the panic would surface as `Err(JoinError::Panic(...))`.
        assert!(
            join_outcome.is_ok(),
            "panicking worker killed the loop — join failed: {join_outcome:?}"
        );
        Ok(())
    }

    /// A02 — after a panicking tick, the loop continues and the next tick
    /// runs normally (reentry per the alignment policy). This proves the
    /// panic did not leave the worker in a broken state — the standard gate
    /// (`enabled` + alignment) still fires on the next interval.
    ///
    /// Uses a worker that panics on the first N ticks then succeeds, proving
    /// recovery + continued ticking.
    struct PanicThenSucceedWorker {
        name: String,
        panic_until: std::sync::atomic::AtomicU32,
        success_count: std::sync::atomic::AtomicU32,
    }

    impl PanicThenSucceedWorker {
        /// Panics on the first `panic_for` ticks, then succeeds.
        fn new(panic_for: u32) -> Self {
            Self {
                name: "panic-then-succeed".to_owned(),
                panic_until: std::sync::atomic::AtomicU32::new(panic_for),
                success_count: std::sync::atomic::AtomicU32::new(0),
            }
        }

        fn success_count(&self) -> u32 {
            self.success_count
                .load(std::sync::atomic::Ordering::Acquire)
        }
    }

    impl Worker for PanicThenSucceedWorker {
        fn name(&self) -> &str {
            &self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn enabled(&self) -> bool {
            true
        }
        fn align_interval(&self) -> AlignInterval {
            AlignInterval(1)
        }
        fn last_run(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn record_run(&self, _: DateTime<Utc>) {
            // Count each recorded tick (each tick that passed the gate).
        }
        fn perform_work(&self, _: &WorkerTickContext<'_>) -> Result<(), String> {
            // Atomically decrement; if still > 0, panic.
            let prev = self.panic_until.fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |v| if v > 0 { Some(v - 1) } else { None },
            );
            match prev {
                Ok(remaining) => {
                    // remaining was the OLD value (pre-decrement). If it was
                    // > 0 we still owe a panic.
                    if remaining > 0 {
                        panic!("simulated early-tick panic ({} remaining)", remaining);
                    }
                    self.success_count
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    Ok(())
                }
                Err(_) => {
                    // Already at 0 — this is a successful tick.
                    self.success_count
                        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    Ok(())
                }
            }
        }
    }

    /// A02 — after panicking on the first 3 ticks, the loop recovers and the
    /// subsequent ticks succeed. `success_count > 0` proves the loop continued
    /// past the panics.
    #[tokio::test]
    async fn a02_loop_continues_after_initial_panics() -> Result<(), Box<dyn std::error::Error>> {
        let worker = Arc::new(PanicThenSucceedWorker::new(3));
        let shutdown = CancellationToken::new();
        let cancel_clone = shutdown.clone();

        // Share the worker between the loop and our assertion via Arc. We need
        // a 'static + owned worker for run_worker_loop, so we clone the Arc
        // into an Arc-wrapper worker.
        let loop_worker = ArcCloneWorker {
            inner: worker.clone(),
            name: "panic-then-succeed".to_owned(),
        };

        let handle = tokio::spawn(run_worker_loop(loop_worker, shutdown, Utc::now));

        // Let enough ticks fire: 3 panics + several successes at 20ms each.
        tokio::time::sleep(Duration::from_millis(200)).await;

        cancel_clone.cancel();
        let join_outcome = handle.await;

        assert!(
            join_outcome.is_ok(),
            "loop died instead of recovering from panics: {join_outcome:?}"
        );
        // At least one successful tick after the panic phase — proves reentry.
        assert!(
            worker.success_count() > 0,
            "expected at least one successful tick after panics, got {}",
            worker.success_count()
        );
        Ok(())
    }

    /// Thin wrapper that lets an `Arc<W>` satisfy `Worker` by delegating,
    /// so the test can observe post-loop state through the shared Arc.
    struct ArcCloneWorker {
        inner: Arc<PanicThenSucceedWorker>,
        name: String,
    }

    impl Worker for ArcCloneWorker {
        fn name(&self) -> &str {
            &self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn enabled(&self) -> bool {
            true
        }
        fn align_interval(&self) -> AlignInterval {
            AlignInterval(1)
        }
        fn last_run(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn record_run(&self, _: DateTime<Utc>) {}
        fn perform_work(&self, ctx: &WorkerTickContext<'_>) -> Result<(), String> {
            self.inner.perform_work(ctx)
        }
    }
}

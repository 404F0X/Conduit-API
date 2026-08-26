//! Pure-logic decisions for Conduit API scheduler workers (RUST-P13-004 S15/S16/S17).
//!
//! Mirrors Go source intent from:
//! * `conduit/internal/server/scheduler/scheduler.go` — `Shutdown()` cancels all
//!   `cancelFunc`s; the underlying executor (`github.com/zhenzou/executors`) runs
//!   fixed-rate/cron callbacks sequentially, so reentry is implicitly prevented
//!   at the executor boundary (no explicit "still running" check in the Go
//!   scheduler itself).
//! * `conduit/internal/server/biz/channel_probe.go` — `shouldRunProbe` is a pure
//!   time-alignment de-duplication function; same-window probes are skipped.
//! * `conduit/internal/server/biz/channel_internal.go` — `shouldRunModelSync`
//!   is the same pattern; it skips when the last execution's aligned bucket
//!   equals the current aligned bucket.
//!
//! The three decision functions below (`decide_run`, `decide_reentry`,
//! `decide_shutdown_transition`) are the Rust projection of Go's behavior:
//! they are pure, take all state as input, and never perform IO. They live
//! alongside the existing `Scheduler`/`JobSpec` lifecycle plumbing in `jobs.rs`
//! (which already implements the runtime wiring for S13-S17 — CancellationToken,
//! per-job enabled flag, non-overlap running-count guard, shutdown-on-drop).
//!
//! [Galileo-the-3rd ?] The Go scheduler has no explicit `decide_reentry`/
//! `decide_shutdown_transition` functions — Go's executor cancels all
//! `cancelFunc`s on shutdown and runs cron callbacks sequentially. The shapes
//! here mirror Go *behavior* (sequential execution => non-reentry; cancel-all
//! on shutdown) rather than literal Go function signatures.

use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::jobs::{SchedulerWorkerSwitches, WorkerJobKind, WorkerJobSwitch};

// ---------------------------------------------------------------------------
// Worker-kind time-alignment intervals — mirror Go enum defaults.
// ---------------------------------------------------------------------------

/// Channel-probe frequency buckets — mirror Go `biz.ProbeFrequency`
/// (`system.go` lines 513-516) and `getIntervalMinutesFromFrequency`
/// (`channel_probe.go` lines 91-103).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFrequency {
    OneMinute,
    FiveMinutes,
    ThirtyMinutes,
    OneHour,
}

impl ProbeFrequency {
    /// Mirrors `getIntervalMinutesFromFrequency` (Go `channel_probe.go:91`).
    pub fn interval_minutes(self) -> i64 {
        match self {
            Self::OneMinute => 1,
            Self::FiveMinutes => 5,
            Self::ThirtyMinutes => 30,
            Self::OneHour => 60,
        }
    }
}

/// Channel model-auto-sync frequency buckets — mirror Go `biz.AutoSyncFrequency`
/// (`system.go` lines 444-446) and `getIntervalMinutesFromAutoSyncFrequency`
/// (`channel_internal.go` lines 48-58).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncFrequency {
    OneHour,
    SixHours,
    OneDay,
}

impl AutoSyncFrequency {
    /// Mirrors `getIntervalMinutesFromAutoSyncFrequency`
    /// (Go `channel_internal.go:48`).
    pub fn interval_minutes(self) -> i64 {
        match self {
            Self::OneHour => 60,
            Self::SixHours => 360,
            Self::OneDay => 1440,
        }
    }
}

/// Auto-backup frequency buckets — mirror Go `biz.BackupFrequency`
/// (used in `backup/autobackup.go` `shouldRunBackup`, lines 74-85).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupFrequency {
    Daily,
    Weekly,
    Monthly,
}

/// Data-storage filesystem-reload interval.
///
/// Mirrors Go `DataStorageService.RegisterScheduledTasks`
/// (`conduit/internal/server/biz/data_storage.go:77-84`) which always registers
/// the cron at `"*/1 * * * *"` — i.e. every minute. Unlike the probe / model-sync
/// workers Go exposes no frequency enum here, so we capture the single canonical
/// 1-minute cadence as a unit-like type. `Minutes` exposes the integer for the
/// `AlignInterval`/`Worker::interval_minutes` projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStorageReloadInterval {
    minutes: i64,
}

impl DataStorageReloadInterval {
    /// The canonical Go cadence — every minute.
    pub const DEFAULT: Self = Self { minutes: 1 };

    /// Build from a raw minute count. The bounded-slice ports only the Go
    /// default (1); the helper exists so a future config-driven cadence can be
    /// wired without touching the worker shape.
    pub const fn from_minutes(minutes: i64) -> Self {
        Self { minutes }
    }

    /// Interval in whole minutes — mirrors the Go `"*/N * * * *"` cron divisor.
    pub const fn interval_minutes(self) -> i64 {
        self.minutes
    }
}

/// Provider-quota check interval — mirrors Go
/// `ProviderQuotaService.getCheckInterval`
/// (`conduit/internal/server/biz/provider_quota.go:388-394`).
///
/// The Go service reads a `provider_quota_check_interval` config knob
/// (a `time.Duration`, see `ProviderQuotaServiceParams.CheckInterval` at
/// `provider_quota.go:251`) and falls back to `5 * time.Minute`. The cron is
/// then derived via `intervalToCronExpr` (`provider_quota.go:336-370`) which
/// snaps the configured minutes onto one of the supported divisors
/// `{1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30, 60}`. We model the same supported
/// set as an enum so the worker's interval is always cron-derivable in Go
/// parity; an out-of-set configured value would map to the nearest lower
/// supported bucket (mirroring Go's rounding) before construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderQuotaCheckInterval {
    /// 1-minute cadence — cron `"*/1 * * * *"`.
    EveryMinute,
    /// 2-minute cadence — cron `"*/2 * * * *"`.
    EveryTwoMinutes,
    /// 3-minute cadence — cron `"*/3 * * * *"`.
    EveryThreeMinutes,
    /// 4-minute cadence — cron `"*/4 * * * *"`.
    EveryFourMinutes,
    /// 5-minute cadence — cron `"*/5 * * * *"` (Go default).
    EveryFiveMinutes,
    /// 6-minute cadence — cron `"*/6 * * * *"`.
    EverySixMinutes,
    /// 10-minute cadence — cron `"*/10 * * * *"`.
    EveryTenMinutes,
    /// 12-minute cadence — cron `"*/12 * * * *"`.
    EveryTwelveMinutes,
    /// 15-minute cadence — cron `"*/15 * * * *"`.
    EveryFifteenMinutes,
    /// 20-minute cadence — cron `"*/20 * * * *"`.
    EveryTwentyMinutes,
    /// 30-minute cadence — cron `"*/30 * * * *"`.
    EveryThirtyMinutes,
    /// 1-hour cadence — cron `"0 * * * *"`.
    EveryHour,
}

impl ProviderQuotaCheckInterval {
    /// Mirrors `getIntervalMinutesFromAutoSyncFrequency` shape — minutes count
    /// used both for `Worker::interval` and `intervalToCronExpr`.
    pub const fn interval_minutes(self) -> i64 {
        match self {
            Self::EveryMinute => 1,
            Self::EveryTwoMinutes => 2,
            Self::EveryThreeMinutes => 3,
            Self::EveryFourMinutes => 4,
            Self::EveryFiveMinutes => 5,
            Self::EverySixMinutes => 6,
            Self::EveryTenMinutes => 10,
            Self::EveryTwelveMinutes => 12,
            Self::EveryFifteenMinutes => 15,
            Self::EveryTwentyMinutes => 20,
            Self::EveryThirtyMinutes => 30,
            Self::EveryHour => 60,
        }
    }

    /// Snap an arbitrary minute count to the nearest *lower-or-equal* supported
    /// interval, mirroring Go's `intervalToCronExpr` rounding branch
    /// (`provider_quota.go:354-369`). Above 60 it snaps to 60.
    pub fn round_from_minutes(requested: i64) -> Self {
        let supported = [1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30, 60];
        let mut chosen = 1i64;
        for &m in supported.iter() {
            if m <= requested {
                chosen = m;
            }
        }
        match chosen {
            2 => Self::EveryTwoMinutes,
            3 => Self::EveryThreeMinutes,
            4 => Self::EveryFourMinutes,
            5 => Self::EveryFiveMinutes,
            6 => Self::EverySixMinutes,
            10 => Self::EveryTenMinutes,
            12 => Self::EveryTwelveMinutes,
            15 => Self::EveryFifteenMinutes,
            20 => Self::EveryTwentyMinutes,
            30 => Self::EveryThirtyMinutes,
            60 => Self::EveryHour,
            _ => Self::EveryMinute,
        }
    }
}

impl Default for ProviderQuotaCheckInterval {
    /// Mirrors Go's `5 * time.Minute` default (`provider_quota.go:393`).
    fn default() -> Self {
        Self::EveryFiveMinutes
    }
}

/// Live-stream registry sweep interval — mirrors Go
/// `LiveStreamRegistry.StartSweeper` (`conduit/internal/server/biz/stream_preview.go:110-124`).
///
/// Go hardcodes `5 * time.Minute` (`stream_preview.go:112`) as the ticker
/// period and `10 * time.Minute` (`stream_preview.go:128`) as the idle-zombie
/// eviction threshold. Like the data-storage reload interval, Go exposes no
/// frequency enum here — we capture the single canonical 5-minute cadence as a
/// unit-like type. `Minutes` exposes the integer for the
/// `AlignInterval`/`Worker::interval_minutes` projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveStreamSweepInterval {
    minutes: i64,
}

impl LiveStreamSweepInterval {
    /// The canonical Go cadence — every 5 minutes
    /// (`stream_preview.go:112`).
    pub const DEFAULT: Self = Self { minutes: 5 };

    /// Build from a raw minute count. The bounded-slice ports only the Go
    /// default (5); the helper exists so a future config-driven cadence can be
    /// wired without touching the worker shape.
    pub const fn from_minutes(minutes: i64) -> Self {
        Self { minutes }
    }

    /// Interval in whole minutes — mirrors the Go `time.NewTicker` period.
    pub const fn interval_minutes(self) -> i64 {
        self.minutes
    }
}

/// Prompt-cache reload interval — mirrors Go `PromptService`
/// (`conduit/internal/server/biz/prompt.go:69-76`).
///
/// Go registers the cron at `"*/1 * * * *"` (`prompt.go:73`) — every minute.
/// Like the data-storage reload interval, Go exposes no frequency enum here; we
/// capture the single canonical 1-minute cadence as a unit-like type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCacheReloadInterval {
    minutes: i64,
}

impl PromptCacheReloadInterval {
    /// The canonical Go cadence — every minute (`prompt.go:73`).
    pub const DEFAULT: Self = Self { minutes: 1 };

    /// Build from a raw minute count. The bounded-slice ports only the Go
    /// default (1); the helper exists so a future config-driven cadence can be
    /// wired without touching the worker shape.
    pub const fn from_minutes(minutes: i64) -> Self {
        Self { minutes }
    }

    /// Interval in whole minutes — mirrors the Go `"*/N * * * *"` cron divisor.
    pub const fn interval_minutes(self) -> i64 {
        self.minutes
    }
}

/// Video-storage scan interval — mirrors Go `video_storage.Worker`
/// (`conduit/internal/server/video_storage/worker.go:54-71`).
///
/// Go reads `settings.ScanIntervalMinutes` and clamps to a minimum of 1 minute
/// (`worker.go:61-64`); the default is 1 minute. We capture the minute-
/// granularity cadence as a unit-like type. `Minutes` exposes the integer for
/// the `AlignInterval`/`Worker::interval_minutes` projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoStorageScanInterval {
    minutes: i64,
}

impl VideoStorageScanInterval {
    /// The Go default cadence — every minute (`worker.go:61-64`, clamped).
    pub const DEFAULT: Self = Self { minutes: 1 };

    /// Build from a raw minute count. The bounded-slice ports only the Go
    /// default (1); the helper exists so a future config-driven cadence can be
    /// wired without touching the worker shape.
    pub const fn from_minutes(minutes: i64) -> Self {
        Self { minutes }
    }

    /// Interval in whole minutes — mirrors Go's `settings.ScanIntervalMinutes`.
    pub const fn interval_minutes(self) -> i64 {
        self.minutes
    }
}

/// Interval (in minutes) to align a wall-clock `DateTime<Utc>` to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlignInterval(pub i64);

impl AlignInterval {
    /// Build an `AlignInterval` from a `Duration` (rounded down to whole minutes).
    pub fn from_duration(duration: Duration) -> Self {
        Self((duration.as_secs() / 60) as i64)
    }

    /// Build from a `ProbeFrequency`.
    pub fn from_probe_frequency(freq: ProbeFrequency) -> Self {
        Self(freq.interval_minutes())
    }

    /// Build from an `AutoSyncFrequency`.
    pub fn from_auto_sync_frequency(freq: AutoSyncFrequency) -> Self {
        Self(freq.interval_minutes())
    }

    /// Build from a `DataStorageReloadInterval` (always 1 minute in Go).
    pub fn from_data_storage_interval(interval: DataStorageReloadInterval) -> Self {
        Self(interval.interval_minutes())
    }

    /// Build from a `ProviderQuotaCheckInterval`.
    pub fn from_provider_quota_interval(interval: ProviderQuotaCheckInterval) -> Self {
        Self(interval.interval_minutes())
    }

    /// Build from a `LiveStreamSweepInterval`.
    pub fn from_live_stream_sweep_interval(interval: LiveStreamSweepInterval) -> Self {
        Self(interval.interval_minutes())
    }

    /// Build from a `PromptCacheReloadInterval`.
    pub fn from_prompt_cache_interval(interval: PromptCacheReloadInterval) -> Self {
        Self(interval.interval_minutes())
    }

    /// Build from a `VideoStorageScanInterval`.
    pub fn from_video_storage_interval(interval: VideoStorageScanInterval) -> Self {
        Self(interval.interval_minutes())
    }
}

/// Align a UTC timestamp to the start of its interval bucket.
///
/// Mirrors Go `now.Truncate(time.Duration(intervalMinutes) * time.Minute)`.
/// Go's `time.Time.Truncate(d)` floors to a multiple of `d` measured from the
/// zero time (year 1, January 1, 00:00:00 UTC), which is equivalent to flooring
/// the wall-clock minute-of-epoch to a multiple of `interval_minutes`. We
/// reproduce that exact semantics by flooring seconds since the Unix epoch.
pub fn align_to_interval(interval: AlignInterval, now: DateTime<Utc>) -> DateTime<Utc> {
    let minutes = interval.0.max(1);
    let total_seconds = now.timestamp();
    let bucket_seconds = minutes * 60;
    let aligned_seconds = total_seconds - total_seconds.rem_euclid(bucket_seconds);
    match DateTime::<Utc>::from_timestamp(aligned_seconds, 0) {
        Some(dt) => dt,
        None => now,
    }
}

/// Decide whether a periodic worker should fire this cycle, based on the
/// time-alignment de-duplication that Go uses for `shouldRunProbe` and
/// `shouldRunModelSync`.
///
/// Mirrors Go (`channel_probe.go:83-88`, `channel_internal.go:33-46`):
/// ```go
/// func shouldRunProbe(frequency ProbeFrequency, now time.Time, lastExecution time.Time) bool {
///     intervalMinutes := getIntervalMinutesFromFrequency(frequency)
///     alignedTime := now.Truncate(time.Duration(intervalMinutes) * time.Minute)
///     return !lastExecution.Equal(alignedTime)
/// }
/// ```
///
/// Returns `true` iff the aligned bucket of `now` differs from `last_run`.
/// A `None` last-run (cold start) always returns `true`.
pub fn should_run_aligned(
    interval: AlignInterval,
    now: DateTime<Utc>,
    last_run: Option<DateTime<Utc>>,
) -> bool {
    let aligned = align_to_interval(interval, now);
    match last_run {
        Some(last) => last != aligned,
        None => true,
    }
}

/// Decide whether an auto-backup should run on the given weekday/day-of-month.
///
/// Mirrors Go `BackupService.shouldRunBackup` (`backup/autobackup.go:74-85`):
/// ```go
/// switch settings.Frequency {
/// case biz.BackupFrequencyDaily:   return true
/// case biz.BackupFrequencyWeekly:  return now.Weekday() == time.Sunday
/// case biz.BackupFrequencyMonthly: return now.Day() == 1
/// default:                          return true
/// }
/// ```
pub fn should_run_backup(frequency: BackupFrequency, now: DateTime<Utc>) -> bool {
    match frequency {
        BackupFrequency::Daily => true,
        BackupFrequency::Weekly => now.weekday() == chrono::Weekday::Sun,
        BackupFrequency::Monthly => now.day() == 1,
    }
}

// ---------------------------------------------------------------------------
// S16 — independent worker switches.
// ---------------------------------------------------------------------------

/// Read-only view over a `SchedulerWorkerSwitches` for per-worker-kind enable
/// queries. Each worker (live-stream-sweeper / channel-probe / model-auto-sync /
/// data-storage / prompt / provider-quota / auto-backup / video-storage) has an
/// independent enable flag in Go — see the per-service `RegisterScheduledTasks`
/// implementations in `biz/*.go` and `video_storage/worker.go`. The GC workers
/// share a parent `enabled` gate (`GcWorkerSwitches.enabled`) plus per-sub-job
/// switches, mirroring the structure Go's config exposes via
/// `StoragePolicy.CleanupOptions` (`system_default.go`).
#[derive(Debug, Clone)]
pub struct WorkerSwitches<'a> {
    inner: &'a SchedulerWorkerSwitches,
}

impl<'a> WorkerSwitches<'a> {
    pub fn new(switches: &'a SchedulerWorkerSwitches) -> Self {
        Self { inner: switches }
    }

    /// Resolve the per-kind switch. Returns `None` for the live-stream-sweeper,
    /// data-storage, and prompt workers — Go has no explicit per-worker enable
    /// flag for those (they always run once registered), so the caller should
    /// treat `None` as "no independent switch — always on".
    pub fn switch(&self, kind: WorkerJobKind) -> Option<&'a WorkerJobSwitch> {
        match kind {
            WorkerJobKind::Backup => Some(&self.inner.backup),
            WorkerJobKind::ProviderQuota => Some(&self.inner.provider_quota),
            WorkerJobKind::VideoStorage => Some(&self.inner.video_storage),
            WorkerJobKind::GcStaleProcessing => Some(&self.inner.gc.stale_processing),
            WorkerJobKind::GcRequestsCleanup => Some(&self.inner.gc.requests_cleanup),
            WorkerJobKind::GcUsageLogsCleanup => Some(&self.inner.gc.usage_logs_cleanup),
        }
    }

    /// Resolve the parent gate (if any) for a worker kind. Only GC sub-jobs have
    /// a parent gate; everything else is independently switched.
    pub fn parent_enabled(&self, kind: WorkerJobKind) -> bool {
        match kind {
            WorkerJobKind::GcStaleProcessing
            | WorkerJobKind::GcRequestsCleanup
            | WorkerJobKind::GcUsageLogsCleanup => self.inner.gc.enabled,
            _ => true,
        }
    }
}

/// Whether a worker kind is independently enabled. Workers with no explicit
/// switch in Go (live-stream-sweeper / data-storage / prompt — i.e. not present
/// in the `WorkerJobKind` enum yet) are considered always-on.
///
/// S16: "channel probe、model sync、auto backup、provider quota、video storage
/// scan 各自独立开关". GC sub-jobs honor both the parent gate and their own
/// per-sub-job switch.
pub fn is_enabled(switches: &SchedulerWorkerSwitches, kind: WorkerJobKind) -> bool {
    let view = WorkerSwitches::new(switches);
    if !view.parent_enabled(kind) {
        return false;
    }
    match view.switch(kind) {
        Some(switch_entry) => switch_entry.enabled(),
        None => true,
    }
}

/// Final run decision for a worker kind, combining the per-kind switch with an
/// optional time-alignment gate. Returns `false` if either the switch is off or
/// (when `now`/`last_run`/`interval` are supplied) the current aligned bucket
/// matches the last run — mirroring Go's combined
/// `enabled && shouldRunProbe/shouldRunModelSync` checks inside each worker's
/// periodic callback.
pub fn decide_run(
    switches: &SchedulerWorkerSwitches,
    kind: WorkerJobKind,
    time_gate: Option<(AlignInterval, DateTime<Utc>, Option<DateTime<Utc>>)>,
) -> bool {
    if !is_enabled(switches, kind) {
        return false;
    }
    match time_gate {
        Some((interval, now, last_run)) => should_run_aligned(interval, now, last_run),
        None => true,
    }
}

// ---------------------------------------------------------------------------
// S15 — non-reentry decision.
// ---------------------------------------------------------------------------

/// Snapshot of one job's runtime state, sufficient to make a pure reentry
/// decision without touching `Scheduler` internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobState {
    /// Whether the job is still executing a previous cycle.
    pub currently_running: bool,
    /// Whether the job declares itself non-overlapping (Go's executor runs
    /// fixed-rate/cron callbacks sequentially, so Go-equivalent is always
    /// `true` — but the Rust `JobSpec` allows per-job opt-out).
    pub non_overlap: bool,
}

/// Outcome of a reentry check.
///
/// [Galileo-the-3rd ?] Go's scheduler has no `Queue` branch — the executor
/// runs callbacks sequentially, so a "still running" cycle is simply *skipped*
/// (the next cron tick re-evaluates). `Queue` is preserved here for parity with
/// the task spec wording ("跳过或排队") but maps to the same observable
/// outcome as `SkipStillRunning` for a sequential executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReentryDecision {
    /// Safe to dispatch the job now.
    Run,
    /// A previous cycle is still running and the job is non-overlapping — skip
    /// this cycle. This is the Go-equivalent default for sequential executors.
    SkipStillRunning,
    /// The job allows overlap (e.g. a long video-storage scan that may queue a
    /// follow-up). Go has no such case in the scheduler path; this branch exists
    /// for forward compatibility with `JobSpec::non_overlap == false`.
    Queue,
}

/// Decide whether to run, skip, or queue a periodic job tick.
///
/// Pure projection of Go's behavior:
/// * If not currently running -> `Run`.
/// * If currently running and non-overlapping -> `SkipStillRunning`.
/// * If currently running and overlap is allowed -> `Queue`.
pub fn decide_reentry(state: JobState) -> ReentryDecision {
    if !state.currently_running {
        ReentryDecision::Run
    } else if state.non_overlap {
        ReentryDecision::SkipStillRunning
    } else {
        ReentryDecision::Queue
    }
}

// ---------------------------------------------------------------------------
// S17 — cancel-on-shutdown transition plan.
// ---------------------------------------------------------------------------

/// Snapshot of the scheduler's running set, used to plan a pure shutdown
/// transition without touching `Scheduler` internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningSnapshot {
    /// Number of jobs currently executing.
    pub running_count: usize,
}

/// Plan returned by `decide_shutdown_transition`. Mirrors Go's
/// `Scheduler.Shutdown(ctx)` (which cancels every `cancelFunc`) plus the fx
/// `OnStop` lifecycle hook that waits for in-flight work to drain — see
/// `scheduler/fx_module.go` lines 14-21 and the per-service `OnStop` hooks in
/// `biz/fx_module.go`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPlan {
    /// No jobs are in flight — cancel all registered `cancelFunc`s and return
    /// immediately. This is the cold-path of Go's `Shutdown`.
    CancelAll,
    /// Jobs are in flight — cancel all `cancelFunc`s (so in-flight work observes
    /// `ctx.Done()`) and wait up to `timeout` for them to drain, matching the
    /// fx `OnStop` hook's bounded context.
    CancelAndWait { timeout: Duration },
}

impl ShutdownPlan {
    /// Effective wait budget for this plan.
    pub fn wait_budget(self) -> Option<Duration> {
        match self {
            Self::CancelAll => None,
            Self::CancelAndWait { timeout } => Some(timeout),
        }
    }

    /// Whether in-flight jobs should be cancelled (always `true` — Go's
    /// `Shutdown` always calls every `cancelFunc`).
    pub fn cancels_in_flight(self) -> bool {
        true
    }
}

/// Decide how the scheduler should transition to shutdown given the current
/// running set and the fx-style stop timeout.
///
/// Pure projection of Go's behavior: shutdown always cancels every registered
/// `cancelFunc`; the fx `OnStop` hook gives the wait budget. If there is nothing
/// in flight the wait is a no-op; otherwise we wait up to `timeout`.
pub fn decide_shutdown_transition(running: RunningSnapshot, timeout: Duration) -> ShutdownPlan {
    if running.running_count == 0 {
        ShutdownPlan::CancelAll
    } else {
        ShutdownPlan::CancelAndWait { timeout }
    }
}

// ---------------------------------------------------------------------------
// Helpers used by tests — exposed for cross-module golden cases.
// ---------------------------------------------------------------------------

/// Construct a UTC `DateTime` from `(year, month, day, hour, minute, second)`.
/// Test-only convenience; lint-safe (no `unwrap`).
pub fn utc_from_ymd_hms(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<DateTime<Utc>> {
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let time = NaiveTime::from_hms_opt(hour, minute, second)?;
    Some(Utc.from_utc_datetime(&date.and_time(time)))
}

#[cfg(test)]
mod tests {
    use super::{
        AlignInterval, AutoSyncFrequency, BackupFrequency, DataStorageReloadInterval, JobState,
        ProbeFrequency, ProviderQuotaCheckInterval, ReentryDecision, RunningSnapshot, ShutdownPlan,
        align_to_interval, decide_reentry, decide_run, decide_shutdown_transition, is_enabled,
        should_run_aligned, should_run_backup, utc_from_ymd_hms,
    };
    use crate::jobs::{GcWorkerSwitches, SchedulerWorkerSwitches, WorkerJobKind, WorkerJobSwitch};
    use std::time::Duration;

    // ---- S16: independent worker switches --------------------------------

    fn switches_with(
        gc_enabled: bool,
        backup_enabled: bool,
        provider_quota_enabled: bool,
        video_storage_enabled: bool,
    ) -> SchedulerWorkerSwitches {
        let interval = Duration::from_secs(60);
        SchedulerWorkerSwitches {
            gc: GcWorkerSwitches {
                enabled: gc_enabled,
                stale_processing: WorkerJobSwitch::new(true, interval),
                requests_cleanup: WorkerJobSwitch::new(true, interval),
                usage_logs_cleanup: WorkerJobSwitch::new(true, interval),
            },
            backup: WorkerJobSwitch::new(backup_enabled, interval),
            provider_quota: WorkerJobSwitch::new(provider_quota_enabled, interval),
            video_storage: WorkerJobSwitch::new(video_storage_enabled, interval),
        }
    }

    #[test]
    fn backup_switch_is_independent_of_provider_quota() {
        let switches = switches_with(
            true, /* backup */ true, /* quota */ false, /* video */ true,
        );
        assert!(is_enabled(&switches, WorkerJobKind::Backup));
        assert!(!is_enabled(&switches, WorkerJobKind::ProviderQuota));
        assert!(is_enabled(&switches, WorkerJobKind::VideoStorage));
    }

    #[test]
    fn gc_parent_gate_disables_all_gc_sub_jobs_only() {
        let switches = switches_with(false, true, true, true);
        assert!(!is_enabled(&switches, WorkerJobKind::GcStaleProcessing));
        assert!(!is_enabled(&switches, WorkerJobKind::GcRequestsCleanup));
        assert!(!is_enabled(&switches, WorkerJobKind::GcUsageLogsCleanup));
        assert!(is_enabled(&switches, WorkerJobKind::Backup));
        assert!(is_enabled(&switches, WorkerJobKind::ProviderQuota));
        assert!(is_enabled(&switches, WorkerJobKind::VideoStorage));
    }

    #[test]
    fn gc_sub_job_per_kind_switch_is_independent() {
        let interval = Duration::from_secs(60);
        let switches = SchedulerWorkerSwitches {
            gc: GcWorkerSwitches {
                enabled: true,
                stale_processing: WorkerJobSwitch::new(false, interval),
                requests_cleanup: WorkerJobSwitch::new(true, interval),
                usage_logs_cleanup: WorkerJobSwitch::new(false, interval),
            },
            backup: WorkerJobSwitch::new(true, interval),
            provider_quota: WorkerJobSwitch::new(true, interval),
            video_storage: WorkerJobSwitch::new(true, interval),
        };
        assert!(!is_enabled(&switches, WorkerJobKind::GcStaleProcessing));
        assert!(is_enabled(&switches, WorkerJobKind::GcRequestsCleanup));
        assert!(!is_enabled(&switches, WorkerJobKind::GcUsageLogsCleanup));
    }

    #[test]
    fn decide_run_respects_switch_before_time_gate() -> Result<(), Box<dyn std::error::Error>> {
        let switches = switches_with(true, false, true, true);
        // Backup disabled — time gate must not even be consulted.
        let now = utc_from_ymd_hms(2024, 1, 1, 10, 0, 0).ok_or("now timestamp")?;
        let decision = decide_run(
            &switches,
            WorkerJobKind::Backup,
            Some((AlignInterval(60), now, None)),
        );
        assert!(!decision);
        Ok(())
    }

    #[test]
    fn decide_run_uses_time_gate_when_switch_on() -> Result<(), Box<dyn std::error::Error>> {
        let switches = switches_with(true, true, true, true);
        let first = utc_from_ymd_hms(2024, 1, 1, 10, 2, 0).ok_or("first timestamp")?;
        let same_window = utc_from_ymd_hms(2024, 1, 1, 10, 59, 0).ok_or("same window timestamp")?;
        let next_window = utc_from_ymd_hms(2024, 1, 1, 11, 0, 0).ok_or("next window timestamp")?;

        // Mirror Go TestChannelService_ShouldRunModelSync_DefaultHourly:
        assert!(decide_run(
            &switches,
            WorkerJobKind::Backup,
            Some((AlignInterval(60), first, None))
        ));
        assert!(!decide_run(
            &switches,
            WorkerJobKind::Backup,
            Some((
                AlignInterval(60),
                same_window,
                Some(align_to_interval(AlignInterval(60), first))
            ))
        ));
        assert!(decide_run(
            &switches,
            WorkerJobKind::Backup,
            Some((
                AlignInterval(60),
                next_window,
                Some(align_to_interval(AlignInterval(60), first))
            ))
        ));
        Ok(())
    }

    // ---- S16: should_run_aligned mirrors Go shouldRunModelSync/shouldRunProbe

    #[test]
    fn should_run_aligned_default_hourly_mirrors_go() -> Result<(), Box<dyn std::error::Error>> {
        // Mirror of Go TestChannelService_ShouldRunModelSync_DefaultHourly.
        let first = utc_from_ymd_hms(2024, 1, 1, 10, 30, 0).ok_or("first")?;
        let same_hour = utc_from_ymd_hms(2024, 1, 1, 10, 59, 0).ok_or("same hour")?;
        let next_hour = utc_from_ymd_hms(2024, 1, 1, 11, 0, 0).ok_or("next hour")?;
        let interval = AlignInterval::from_auto_sync_frequency(AutoSyncFrequency::OneHour);

        assert!(should_run_aligned(interval, first, None));
        assert!(!should_run_aligned(
            interval,
            same_hour,
            Some(align_to_interval(interval, first))
        ));
        assert!(should_run_aligned(
            interval,
            next_hour,
            Some(align_to_interval(interval, first))
        ));
        Ok(())
    }

    #[test]
    fn should_run_aligned_six_hour_window_mirrors_go() -> Result<(), Box<dyn std::error::Error>> {
        // Mirror of Go TestChannelService_ShouldRunModelSync_SixHourInterval.
        let first = utc_from_ymd_hms(2024, 1, 1, 10, 30, 0).ok_or("first")?;
        let same_window = utc_from_ymd_hms(2024, 1, 1, 11, 59, 0).ok_or("same window")?;
        let next_window = utc_from_ymd_hms(2024, 1, 1, 12, 0, 0).ok_or("next window")?;
        let interval = AlignInterval::from_auto_sync_frequency(AutoSyncFrequency::SixHours);

        assert!(should_run_aligned(interval, first, None));
        assert!(!should_run_aligned(
            interval,
            same_window,
            Some(align_to_interval(interval, first))
        ));
        assert!(should_run_aligned(
            interval,
            next_window,
            Some(align_to_interval(interval, first))
        ));
        Ok(())
    }

    #[test]
    fn should_run_aligned_daily_window_mirrors_go() -> Result<(), Box<dyn std::error::Error>> {
        // Mirror of Go TestChannelService_ShouldRunModelSync_DailyInterval.
        let first = utc_from_ymd_hms(2024, 1, 1, 10, 30, 0).ok_or("first")?;
        let same_window = utc_from_ymd_hms(2024, 1, 1, 23, 59, 0).ok_or("same window")?;
        let next_window = utc_from_ymd_hms(2024, 1, 2, 0, 0, 0).ok_or("next window")?;
        let interval = AlignInterval::from_auto_sync_frequency(AutoSyncFrequency::OneDay);

        assert!(should_run_aligned(interval, first, None));
        assert!(!should_run_aligned(
            interval,
            same_window,
            Some(align_to_interval(interval, first))
        ));
        assert!(should_run_aligned(
            interval,
            next_window,
            Some(align_to_interval(interval, first))
        ));
        Ok(())
    }

    #[test]
    fn should_run_aligned_probe_one_minute_mirrors_go() -> Result<(), Box<dyn std::error::Error>> {
        // Mirror of Go shouldRunProbe (channel_probe.go:83) at 1m frequency:
        // a probe at 10:02:30 and another tick at 10:02:45 share the same
        // aligned bucket (10:02:00), so the second is skipped.
        let interval = AlignInterval::from_probe_frequency(ProbeFrequency::OneMinute);
        let first_tick = utc_from_ymd_hms(2024, 1, 1, 10, 2, 0).ok_or("first tick")?;
        let same_minute = utc_from_ymd_hms(2024, 1, 1, 10, 2, 45).ok_or("same minute")?;

        // Cold start always runs.
        assert!(should_run_aligned(interval, first_tick, None));
        let first_bucket = align_to_interval(interval, first_tick);
        // Same bucket as a prior run -> skip (Go's non-reentry via alignment).
        assert!(!should_run_aligned(
            interval,
            same_minute,
            Some(first_bucket)
        ));
        assert_eq!(align_to_interval(interval, same_minute), first_bucket);

        // Cross into the next minute bucket -> run.
        let next_minute = utc_from_ymd_hms(2024, 1, 1, 10, 3, 0).ok_or("next minute")?;
        assert_ne!(align_to_interval(interval, next_minute), first_bucket);
        assert!(should_run_aligned(
            interval,
            next_minute,
            Some(first_bucket)
        ));
        Ok(())
    }

    #[test]
    fn probe_frequency_interval_minutes_mirrors_go() {
        assert_eq!(ProbeFrequency::OneMinute.interval_minutes(), 1);
        assert_eq!(ProbeFrequency::FiveMinutes.interval_minutes(), 5);
        assert_eq!(ProbeFrequency::ThirtyMinutes.interval_minutes(), 30);
        assert_eq!(ProbeFrequency::OneHour.interval_minutes(), 60);
    }

    #[test]
    fn auto_sync_frequency_interval_minutes_mirrors_go() {
        assert_eq!(AutoSyncFrequency::OneHour.interval_minutes(), 60);
        assert_eq!(AutoSyncFrequency::SixHours.interval_minutes(), 360);
        assert_eq!(AutoSyncFrequency::OneDay.interval_minutes(), 1440);
    }

    #[test]
    fn data_storage_reload_interval_default_is_one_minute() {
        // Go data_storage.go:81 registers the cron at `"*/1 * * * *"`.
        assert_eq!(DataStorageReloadInterval::DEFAULT.interval_minutes(), 1);
        assert_eq!(
            DataStorageReloadInterval::from_minutes(1).interval_minutes(),
            1
        );
    }

    #[test]
    fn provider_quota_check_interval_default_mirrors_go() {
        // Go provider_quota.go:393 fallback is `5 * time.Minute`.
        assert_eq!(ProviderQuotaCheckInterval::default().interval_minutes(), 5);
    }

    #[test]
    fn provider_quota_check_interval_minutes_mirrors_go_supported_set() {
        // Go provider_quota.go:355 supportedIntervals.
        let cases = [
            (ProviderQuotaCheckInterval::EveryMinute, 1),
            (ProviderQuotaCheckInterval::EveryTwoMinutes, 2),
            (ProviderQuotaCheckInterval::EveryThreeMinutes, 3),
            (ProviderQuotaCheckInterval::EveryFourMinutes, 4),
            (ProviderQuotaCheckInterval::EveryFiveMinutes, 5),
            (ProviderQuotaCheckInterval::EverySixMinutes, 6),
            (ProviderQuotaCheckInterval::EveryTenMinutes, 10),
            (ProviderQuotaCheckInterval::EveryTwelveMinutes, 12),
            (ProviderQuotaCheckInterval::EveryFifteenMinutes, 15),
            (ProviderQuotaCheckInterval::EveryTwentyMinutes, 20),
            (ProviderQuotaCheckInterval::EveryThirtyMinutes, 30),
            (ProviderQuotaCheckInterval::EveryHour, 60),
        ];
        for (variant, minutes) in cases {
            assert_eq!(
                variant.interval_minutes(),
                minutes,
                "ProviderQuotaCheckInterval::{variant:?} should map to {minutes} minutes"
            );
        }
    }

    #[test]
    fn provider_quota_round_from_minutes_mirrors_go_rounding() {
        // Go provider_quota.go:354-369 — snaps to nearest lower supported
        // interval from {1,2,3,4,5,6,10,12,15,20,30,60}.
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(0),
            ProviderQuotaCheckInterval::EveryMinute
        );
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(1),
            ProviderQuotaCheckInterval::EveryMinute
        );
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(5),
            ProviderQuotaCheckInterval::EveryFiveMinutes
        );
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(7),
            ProviderQuotaCheckInterval::EverySixMinutes
        );
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(45),
            ProviderQuotaCheckInterval::EveryThirtyMinutes
        );
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(60),
            ProviderQuotaCheckInterval::EveryHour
        );
        // Above 60 snaps to 60 (Go's `rounded = 60` fallback).
        assert_eq!(
            ProviderQuotaCheckInterval::round_from_minutes(120),
            ProviderQuotaCheckInterval::EveryHour
        );
    }

    #[test]
    fn should_run_backup_mirrors_go_should_run_backup() -> Result<(), Box<dyn std::error::Error>> {
        // Go autobackup.go:74-85.
        let any_day = utc_from_ymd_hms(2024, 3, 15, 2, 0, 0).ok_or("mid-month")?;
        let sunday = utc_from_ymd_hms(2024, 1, 7, 2, 0, 0).ok_or("first Sunday of 2024")?;
        let monday = utc_from_ymd_hms(2024, 1, 8, 2, 0, 0).ok_or("first Monday of 2024")?;
        let first_of_month = utc_from_ymd_hms(2024, 3, 1, 2, 0, 0).ok_or("first of month")?;

        assert!(should_run_backup(BackupFrequency::Daily, any_day));
        assert!(should_run_backup(BackupFrequency::Weekly, sunday));
        assert!(!should_run_backup(BackupFrequency::Weekly, monday));
        assert!(should_run_backup(BackupFrequency::Monthly, first_of_month));
        assert!(!should_run_backup(BackupFrequency::Monthly, any_day));
        Ok(())
    }

    // ---- S15: non-reentry -------------------------------------------------

    #[test]
    fn reentry_runs_when_idle() {
        assert_eq!(
            decide_reentry(JobState {
                currently_running: false,
                non_overlap: true,
            }),
            ReentryDecision::Run
        );
    }

    #[test]
    fn reentry_skips_when_running_and_non_overlap() {
        // Go default: sequential executor => non-overlap => skip.
        assert_eq!(
            decide_reentry(JobState {
                currently_running: true,
                non_overlap: true,
            }),
            ReentryDecision::SkipStillRunning
        );
    }

    #[test]
    fn reentry_queues_when_overlap_allowed() {
        // Forward-compat for JobSpec::non_overlap == false (no Go equivalent).
        assert_eq!(
            decide_reentry(JobState {
                currently_running: true,
                non_overlap: false,
            }),
            ReentryDecision::Queue
        );
    }

    #[test]
    fn reentry_idle_runs_even_when_overlap_allowed() {
        assert_eq!(
            decide_reentry(JobState {
                currently_running: false,
                non_overlap: false,
            }),
            ReentryDecision::Run
        );
    }

    // ---- S17: cancel-on-shutdown transition ------------------------------

    #[test]
    fn shutdown_with_no_running_jobs_cancels_immediately() {
        let plan = decide_shutdown_transition(
            RunningSnapshot { running_count: 0 },
            Duration::from_secs(30),
        );
        assert_eq!(plan, ShutdownPlan::CancelAll);
        assert!(plan.cancels_in_flight());
        assert_eq!(plan.wait_budget(), None);
    }

    #[test]
    fn shutdown_with_running_jobs_waits_bounded() {
        let timeout = Duration::from_secs(30);
        let plan = decide_shutdown_transition(RunningSnapshot { running_count: 3 }, timeout);
        assert_eq!(plan, ShutdownPlan::CancelAndWait { timeout });
        assert!(plan.cancels_in_flight());
        assert_eq!(plan.wait_budget(), Some(timeout));
    }

    #[test]
    fn shutdown_plan_always_cancels_in_flight() {
        // Go's Shutdown always calls every cancelFunc — even with zero running.
        let plan_a = decide_shutdown_transition(
            RunningSnapshot { running_count: 0 },
            Duration::from_secs(1),
        );
        let plan_b = decide_shutdown_transition(
            RunningSnapshot { running_count: 5 },
            Duration::from_secs(1),
        );
        assert!(plan_a.cancels_in_flight());
        assert!(plan_b.cancels_in_flight());
    }
}

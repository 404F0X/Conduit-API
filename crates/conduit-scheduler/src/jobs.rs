use std::{
    collections::HashMap,
    error::Error,
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{sync::Notify, task::JoinHandle};

type JobFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type JobRunner = Arc<dyn Fn(JobContext) -> JobFuture + Send + Sync + 'static>;

pub const JOB_GC_STALE_PROCESSING: &str = "gc.stale_processing";
pub const JOB_GC_REQUESTS_CLEANUP: &str = "gc.requests_cleanup";
pub const JOB_GC_USAGE_LOGS_CLEANUP: &str = "gc.usage_logs_cleanup";
pub const JOB_BACKUP: &str = "backup";
pub const JOB_PROVIDER_QUOTA: &str = "provider_quota";
pub const JOB_VIDEO_STORAGE: &str = "video_storage";

#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

struct CancellationState {
    cancelled: AtomicBool,
    notify: Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
            }),
        }
    }

    pub fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::AcqRel) {
            self.inner.notify.notify_waiters();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }

            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct JobContext {
    job_name: Arc<str>,
    shutdown: CancellationToken,
}

impl JobContext {
    pub fn job_name(&self) -> &str {
        &self.job_name
    }

    pub fn shutdown(&self) -> &CancellationToken {
        &self.shutdown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkerJobKind {
    GcStaleProcessing,
    GcRequestsCleanup,
    GcUsageLogsCleanup,
    Backup,
    ProviderQuota,
    VideoStorage,
}

impl WorkerJobKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::GcStaleProcessing => JOB_GC_STALE_PROCESSING,
            Self::GcRequestsCleanup => JOB_GC_REQUESTS_CLEANUP,
            Self::GcUsageLogsCleanup => JOB_GC_USAGE_LOGS_CLEANUP,
            Self::Backup => JOB_BACKUP,
            Self::ProviderQuota => JOB_PROVIDER_QUOTA,
            Self::VideoStorage => JOB_VIDEO_STORAGE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerJobSwitch {
    enabled: bool,
    interval: Duration,
    non_overlap: bool,
}

impl WorkerJobSwitch {
    pub fn new(enabled: bool, interval: Duration) -> Self {
        Self {
            enabled,
            interval,
            non_overlap: true,
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    pub fn with_non_overlap(mut self, non_overlap: bool) -> Self {
        self.non_overlap = non_overlap;
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn non_overlap(&self) -> bool {
        self.non_overlap
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcWorkerSwitches {
    pub enabled: bool,
    pub stale_processing: WorkerJobSwitch,
    pub requests_cleanup: WorkerJobSwitch,
    pub usage_logs_cleanup: WorkerJobSwitch,
}

impl Default for GcWorkerSwitches {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_processing: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            requests_cleanup: WorkerJobSwitch::new(false, Duration::from_secs(24 * 60 * 60)),
            usage_logs_cleanup: WorkerJobSwitch::new(false, Duration::from_secs(24 * 60 * 60)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerWorkerSwitches {
    pub gc: GcWorkerSwitches,
    pub backup: WorkerJobSwitch,
    pub provider_quota: WorkerJobSwitch,
    pub video_storage: WorkerJobSwitch,
}

impl Default for SchedulerWorkerSwitches {
    fn default() -> Self {
        Self {
            gc: GcWorkerSwitches::default(),
            backup: WorkerJobSwitch::new(false, Duration::from_secs(24 * 60 * 60)),
            provider_quota: WorkerJobSwitch::new(true, Duration::from_secs(10 * 60)),
            video_storage: WorkerJobSwitch::new(true, Duration::from_secs(60)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPlanEntry {
    kind: WorkerJobKind,
    enabled: bool,
    interval: Duration,
    non_overlap: bool,
}

impl WorkerPlanEntry {
    fn from_switch(kind: WorkerJobKind, parent_enabled: bool, switch: &WorkerJobSwitch) -> Self {
        Self {
            kind,
            enabled: parent_enabled && switch.enabled(),
            interval: switch.interval(),
            non_overlap: switch.non_overlap(),
        }
    }

    pub fn kind(&self) -> WorkerJobKind {
        self.kind
    }

    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn non_overlap(&self) -> bool {
        self.non_overlap
    }

    pub fn spec_with_runner<F, Fut>(&self, runner: F) -> JobSpec
    where
        F: Fn(JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        JobSpec::new(self.name(), self.interval, runner)
            .with_enabled(self.enabled)
            .with_non_overlap(self.non_overlap)
    }

    pub fn noop_spec(&self) -> JobSpec {
        self.spec_with_runner(|_| async {})
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPlan {
    entries: Vec<WorkerPlanEntry>,
}

impl WorkerPlan {
    pub fn from_switches(switches: &SchedulerWorkerSwitches) -> Self {
        Self {
            entries: vec![
                WorkerPlanEntry::from_switch(
                    WorkerJobKind::GcStaleProcessing,
                    switches.gc.enabled,
                    &switches.gc.stale_processing,
                ),
                WorkerPlanEntry::from_switch(
                    WorkerJobKind::GcRequestsCleanup,
                    switches.gc.enabled,
                    &switches.gc.requests_cleanup,
                ),
                WorkerPlanEntry::from_switch(
                    WorkerJobKind::GcUsageLogsCleanup,
                    switches.gc.enabled,
                    &switches.gc.usage_logs_cleanup,
                ),
                WorkerPlanEntry::from_switch(WorkerJobKind::Backup, true, &switches.backup),
                WorkerPlanEntry::from_switch(
                    WorkerJobKind::ProviderQuota,
                    true,
                    &switches.provider_quota,
                ),
                WorkerPlanEntry::from_switch(
                    WorkerJobKind::VideoStorage,
                    true,
                    &switches.video_storage,
                ),
            ],
        }
    }

    pub fn entries(&self) -> &[WorkerPlanEntry] {
        &self.entries
    }

    pub fn register_noop_jobs(
        &self,
        scheduler: &mut Scheduler,
    ) -> Result<(), SchedulerRegisterError> {
        for entry in &self.entries {
            scheduler.register_job(entry.noop_spec())?;
        }

        Ok(())
    }
}

impl Default for WorkerPlan {
    fn default() -> Self {
        Self::from_switches(&SchedulerWorkerSwitches::default())
    }
}

#[derive(Clone)]
pub struct JobSpec {
    name: Arc<str>,
    enabled: bool,
    interval: Duration,
    non_overlap: bool,
    runner: JobRunner,
}

impl JobSpec {
    pub fn new<N, F, Fut>(name: N, interval: Duration, runner: F) -> Self
    where
        N: Into<Arc<str>>,
        F: Fn(JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self {
            name: name.into(),
            enabled: true,
            interval,
            non_overlap: true,
            runner: Arc::new(move |context| Box::pin(runner(context))),
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_non_overlap(mut self, non_overlap: bool) -> Self {
        self.non_overlap = non_overlap;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub fn non_overlap(&self) -> bool {
        self.non_overlap
    }
}

impl fmt::Debug for JobSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JobSpec")
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("interval", &self.interval)
            .field("non_overlap", &self.non_overlap)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct JobHandle {
    spec: Arc<JobSpec>,
    running_count: Arc<AtomicUsize>,
}

impl JobHandle {
    fn new(spec: JobSpec) -> Self {
        Self {
            spec: Arc::new(spec),
            running_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn spec(&self) -> &JobSpec {
        &self.spec
    }

    pub fn is_running(&self) -> bool {
        self.running_count.load(Ordering::Acquire) > 0
    }
}

#[derive(Debug)]
struct RunningPermit {
    running_count: Arc<AtomicUsize>,
}

impl Drop for RunningPermit {
    fn drop(&mut self) {
        self.running_count.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRegistryError {
    DuplicateName(String),
    EmptyName,
    ZeroInterval(String),
}

impl fmt::Display for JobRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateName(name) => write!(formatter, "job '{name}' is already registered"),
            Self::EmptyName => formatter.write_str("job name cannot be empty"),
            Self::ZeroInterval(name) => write!(formatter, "job '{name}' interval cannot be zero"),
        }
    }
}

impl Error for JobRegistryError {}

#[derive(Default, Debug, Clone)]
pub struct JobRegistry {
    jobs: HashMap<String, JobHandle>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: JobSpec) -> Result<(), JobRegistryError> {
        let name = spec.name().to_owned();
        if name.is_empty() {
            return Err(JobRegistryError::EmptyName);
        }
        if spec.interval().is_zero() {
            return Err(JobRegistryError::ZeroInterval(name));
        }
        if self.jobs.contains_key(&name) {
            return Err(JobRegistryError::DuplicateName(name));
        }

        self.jobs.insert(name, JobHandle::new(spec));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&JobHandle> {
        self.jobs.get(name)
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.jobs.keys().map(String::as_str)
    }

    fn handles(&self) -> Vec<JobHandle> {
        self.jobs.values().cloned().collect()
    }
}

#[derive(Debug)]
pub enum SchedulerRegisterError {
    Registry(JobRegistryError),
    ShuttingDown,
}

impl fmt::Display for SchedulerRegisterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(formatter),
            Self::ShuttingDown => formatter.write_str("scheduler is shutting down"),
        }
    }
}

impl Error for SchedulerRegisterError {}

impl From<JobRegistryError> for SchedulerRegisterError {
    fn from(error: JobRegistryError) -> Self {
        Self::Registry(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobSkipReason {
    AlreadyRunning,
    Disabled,
}

#[derive(Debug)]
pub enum JobRun {
    Started(JoinHandle<()>),
    Skipped(JobSkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRunError {
    NotFound(String),
    ShuttingDown,
}

impl fmt::Display for JobRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(formatter, "job '{name}' is not registered"),
            Self::ShuttingDown => formatter.write_str("scheduler is shutting down"),
        }
    }
}

impl Error for JobRunError {}

#[derive(Debug, Default)]
pub struct Scheduler {
    registry: JobRegistry,
    shutdown: CancellationToken,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_job(&mut self, spec: JobSpec) -> Result<(), SchedulerRegisterError> {
        if self.shutdown.is_cancelled() {
            return Err(SchedulerRegisterError::ShuttingDown);
        }

        self.registry.register(spec)?;
        Ok(())
    }

    pub fn registry(&self) -> &JobRegistry {
        &self.registry
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    pub fn run_job_now(&self, name: &str) -> Result<JobRun, JobRunError> {
        let handle = self
            .registry
            .get(name)
            .ok_or_else(|| JobRunError::NotFound(name.to_owned()))?
            .clone();

        start_job(handle, self.shutdown.clone())
    }

    pub fn start(&self) -> SchedulerWorkers {
        let mut joins = Vec::new();

        for handle in self.registry.handles() {
            if !handle.spec().enabled() {
                continue;
            }

            let shutdown = self.shutdown.clone();
            joins.push(tokio::spawn(async move {
                let mut interval = tokio::time::interval(handle.spec().interval());

                loop {
                    tokio::select! {
                        () = shutdown.cancelled() => break,
                        _ = interval.tick() => {
                            // Interval workers only submit a run; non-overlap is enforced in start_job.
                            let _ = start_job(handle.clone(), shutdown.clone());
                        }
                    }
                }
            }));
        }

        SchedulerWorkers {
            shutdown: self.shutdown.clone(),
            joins,
        }
    }
}

#[derive(Debug)]
pub struct SchedulerWorkers {
    shutdown: CancellationToken,
    joins: Vec<JoinHandle<()>>,
}

impl SchedulerWorkers {
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn shutdown(mut self) {
        self.shutdown.cancel();

        while let Some(join) = self.joins.pop() {
            let _join_result = join.await;
        }
    }
}

fn start_job(handle: JobHandle, shutdown: CancellationToken) -> Result<JobRun, JobRunError> {
    if shutdown.is_cancelled() {
        return Err(JobRunError::ShuttingDown);
    }
    if !handle.spec().enabled() {
        return Ok(JobRun::Skipped(JobSkipReason::Disabled));
    }

    if handle.spec().non_overlap() {
        let acquired =
            handle
                .running_count
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
        if acquired.is_err() {
            return Ok(JobRun::Skipped(JobSkipReason::AlreadyRunning));
        }
    } else {
        handle.running_count.fetch_add(1, Ordering::AcqRel);
    }

    // The permit clears the running marker even if the spawned task is aborted or panics.
    let permit = RunningPermit {
        running_count: handle.running_count.clone(),
    };
    let runner = handle.spec().runner.clone();
    let context = JobContext {
        job_name: handle.spec().name.clone(),
        shutdown,
    };

    let join = tokio::spawn(async move {
        let _permit = permit;
        runner(context).await;
    });

    Ok(JobRun::Started(join))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::Notify;

    use super::{
        GcWorkerSwitches, JOB_BACKUP, JOB_GC_REQUESTS_CLEANUP, JOB_GC_STALE_PROCESSING,
        JOB_GC_USAGE_LOGS_CLEANUP, JOB_PROVIDER_QUOTA, JOB_VIDEO_STORAGE, JobRegistryError, JobRun,
        JobRunError, JobSkipReason, JobSpec, Scheduler, SchedulerRegisterError,
        SchedulerWorkerSwitches, WorkerJobKind, WorkerJobSwitch, WorkerPlan,
    };

    #[tokio::test]
    async fn registers_jobs_in_registry() {
        let mut scheduler = Scheduler::new();

        let registration =
            scheduler.register_job(JobSpec::new("fake", Duration::from_secs(60), |_| async {}));

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 1);
        assert!(scheduler.registry().get("fake").is_some());
        assert!(scheduler.registry().names().any(|name| name == "fake"));
    }

    #[tokio::test]
    async fn rejects_duplicate_job_names() {
        let mut scheduler = Scheduler::new();

        let first =
            scheduler.register_job(JobSpec::new("fake", Duration::from_secs(60), |_| async {}));
        let second =
            scheduler.register_job(JobSpec::new("fake", Duration::from_secs(60), |_| async {}));

        assert!(first.is_ok());
        assert!(matches!(
            second,
            Err(SchedulerRegisterError::Registry(
                JobRegistryError::DuplicateName(name)
            )) if name == "fake"
        ));
    }

    #[tokio::test]
    async fn skips_non_overlapping_job_reentry() {
        let mut scheduler = Scheduler::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let run_count = Arc::new(AtomicUsize::new(0));

        let entered_for_job = entered.clone();
        let release_for_job = release.clone();
        let run_count_for_job = run_count.clone();
        let registration = scheduler.register_job(JobSpec::new(
            "blocking",
            Duration::from_secs(60),
            move |_| {
                let entered = entered_for_job.clone();
                let release = release_for_job.clone();
                let run_count = run_count_for_job.clone();
                async move {
                    run_count.fetch_add(1, Ordering::AcqRel);
                    entered.notify_one();
                    release.notified().await;
                }
            },
        ));
        assert!(registration.is_ok());

        let first_join = match scheduler.run_job_now("blocking") {
            Ok(JobRun::Started(join)) => join,
            other => panic!("expected first run to start, got {other:?}"),
        };

        let entered_result = tokio::time::timeout(Duration::from_secs(1), entered.notified()).await;
        assert!(entered_result.is_ok());

        let second = scheduler.run_job_now("blocking");
        assert!(matches!(
            second,
            Ok(JobRun::Skipped(JobSkipReason::AlreadyRunning))
        ));

        release.notify_waiters();
        let join_result = first_join.await;
        assert!(join_result.is_ok());
        assert_eq!(run_count.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn disabled_job_is_skipped() {
        let mut scheduler = Scheduler::new();
        let run_count = Arc::new(AtomicUsize::new(0));
        let run_count_for_job = run_count.clone();

        let registration = scheduler.register_job(
            JobSpec::new("disabled", Duration::from_secs(60), move |_| {
                let run_count = run_count_for_job.clone();
                async move {
                    run_count.fetch_add(1, Ordering::AcqRel);
                }
            })
            .with_enabled(false),
        );
        assert!(registration.is_ok());

        let run = scheduler.run_job_now("disabled");

        assert!(matches!(run, Ok(JobRun::Skipped(JobSkipReason::Disabled))));
        assert_eq!(run_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn worker_plan_keeps_job_switches_independent() -> Result<(), Box<dyn std::error::Error>> {
        let interval = Duration::from_secs(30);
        let switches = SchedulerWorkerSwitches {
            gc: GcWorkerSwitches {
                enabled: true,
                stale_processing: WorkerJobSwitch::new(false, interval),
                requests_cleanup: WorkerJobSwitch::new(true, interval).with_non_overlap(false),
                usage_logs_cleanup: WorkerJobSwitch::new(false, interval),
            },
            backup: WorkerJobSwitch::new(true, interval),
            provider_quota: WorkerJobSwitch::new(false, interval),
            video_storage: WorkerJobSwitch::new(true, interval),
        };

        let plan = WorkerPlan::from_switches(&switches);
        let entry = |kind| {
            plan.entries()
                .iter()
                .find(|entry| entry.kind() == kind)
                .ok_or("planned worker job entry")
        };

        assert_eq!(plan.entries().len(), 6);
        assert!(!entry(WorkerJobKind::GcStaleProcessing)?.enabled());
        assert!(entry(WorkerJobKind::GcRequestsCleanup)?.enabled());
        assert!(!entry(WorkerJobKind::GcRequestsCleanup)?.non_overlap());
        assert!(!entry(WorkerJobKind::GcUsageLogsCleanup)?.enabled());
        assert!(entry(WorkerJobKind::Backup)?.enabled());
        assert!(!entry(WorkerJobKind::ProviderQuota)?.enabled());
        assert!(entry(WorkerJobKind::VideoStorage)?.enabled());
        assert!(entry(WorkerJobKind::Backup)?.non_overlap());
        assert_eq!(entry(WorkerJobKind::Backup)?.interval(), interval);
        Ok(())
    }

    #[test]
    fn gc_parent_switch_disables_only_gc_plan_entries() {
        let switches = SchedulerWorkerSwitches {
            gc: GcWorkerSwitches {
                enabled: false,
                stale_processing: WorkerJobSwitch::new(true, Duration::from_secs(60)),
                requests_cleanup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
                usage_logs_cleanup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            },
            backup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            provider_quota: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            video_storage: WorkerJobSwitch::new(true, Duration::from_secs(60)),
        };

        let plan = WorkerPlan::from_switches(&switches);
        let enabled_names = plan
            .entries()
            .iter()
            .filter(|entry| entry.enabled())
            .map(|entry| entry.name())
            .collect::<Vec<_>>();

        assert_eq!(
            enabled_names,
            vec![JOB_BACKUP, JOB_PROVIDER_QUOTA, JOB_VIDEO_STORAGE]
        );
        assert!(
            plan.entries()
                .iter()
                .filter(|entry| entry.name().starts_with("gc."))
                .all(|entry| !entry.enabled())
        );
    }

    #[tokio::test]
    async fn worker_plan_registers_disabled_jobs_and_preserves_non_overlap_per_job()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scheduler = Scheduler::new();
        let switches = SchedulerWorkerSwitches {
            gc: GcWorkerSwitches {
                enabled: false,
                stale_processing: WorkerJobSwitch::new(true, Duration::from_secs(60)),
                requests_cleanup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
                usage_logs_cleanup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            },
            backup: WorkerJobSwitch::new(true, Duration::from_secs(60)),
            provider_quota: WorkerJobSwitch::new(false, Duration::from_secs(60)),
            video_storage: WorkerJobSwitch::new(true, Duration::from_secs(60)),
        };
        let plan = WorkerPlan::from_switches(&switches);
        let backup_entered = Arc::new(Notify::new());
        let backup_release = Arc::new(Notify::new());
        let backup_count = Arc::new(AtomicUsize::new(0));
        let video_count = Arc::new(AtomicUsize::new(0));

        for entry in plan.entries() {
            let backup_entered_for_job = backup_entered.clone();
            let backup_release_for_job = backup_release.clone();
            let backup_count_for_job = backup_count.clone();
            let video_count_for_job = video_count.clone();
            let registration = scheduler.register_job(entry.spec_with_runner(move |context| {
                let backup_entered = backup_entered_for_job.clone();
                let backup_release = backup_release_for_job.clone();
                let backup_count = backup_count_for_job.clone();
                let video_count = video_count_for_job.clone();
                async move {
                    match context.job_name() {
                        JOB_BACKUP => {
                            backup_count.fetch_add(1, Ordering::AcqRel);
                            backup_entered.notify_one();
                            backup_release.notified().await;
                        }
                        JOB_VIDEO_STORAGE => {
                            video_count.fetch_add(1, Ordering::AcqRel);
                        }
                        _ => {}
                    }
                }
            }));
            assert!(registration.is_ok());
        }

        assert_eq!(scheduler.registry().len(), 6);
        assert!(
            !scheduler
                .registry()
                .get(JOB_PROVIDER_QUOTA)
                .ok_or("provider quota job")?
                .spec()
                .enabled()
        );
        assert!(
            scheduler
                .registry()
                .get(JOB_BACKUP)
                .ok_or("backup job")?
                .spec()
                .non_overlap()
        );

        let backup_join = match scheduler.run_job_now(JOB_BACKUP) {
            Ok(JobRun::Started(join)) => join,
            other => panic!("expected backup run to start, got {other:?}"),
        };
        let entered_result =
            tokio::time::timeout(Duration::from_secs(1), backup_entered.notified()).await;
        assert!(entered_result.is_ok());

        let second_backup = scheduler.run_job_now(JOB_BACKUP);
        assert!(matches!(
            second_backup,
            Ok(JobRun::Skipped(JobSkipReason::AlreadyRunning))
        ));

        let provider_quota = scheduler.run_job_now(JOB_PROVIDER_QUOTA);
        assert!(matches!(
            provider_quota,
            Ok(JobRun::Skipped(JobSkipReason::Disabled))
        ));

        let video_join = match scheduler.run_job_now(JOB_VIDEO_STORAGE) {
            Ok(JobRun::Started(join)) => join,
            other => panic!("expected video storage run to start, got {other:?}"),
        };
        assert!(video_join.await.is_ok());

        backup_release.notify_waiters();
        assert!(backup_join.await.is_ok());
        assert_eq!(backup_count.load(Ordering::Acquire), 1);
        assert_eq!(video_count.load(Ordering::Acquire), 1);
        Ok(())
    }

    #[tokio::test]
    async fn worker_plan_registers_noop_skeleton_jobs() {
        let mut scheduler = Scheduler::new();
        let plan = WorkerPlan::default();

        let registration = plan.register_noop_jobs(&mut scheduler);

        assert!(registration.is_ok());
        assert_eq!(scheduler.registry().len(), 6);
        assert!(scheduler.registry().get(JOB_GC_STALE_PROCESSING).is_some());
        assert!(scheduler.registry().get(JOB_GC_REQUESTS_CLEANUP).is_some());
        assert!(
            scheduler
                .registry()
                .get(JOB_GC_USAGE_LOGS_CLEANUP)
                .is_some()
        );
        assert!(scheduler.registry().get(JOB_BACKUP).is_some());
        assert!(scheduler.registry().get(JOB_PROVIDER_QUOTA).is_some());
        assert!(scheduler.registry().get(JOB_VIDEO_STORAGE).is_some());
    }

    #[tokio::test]
    async fn shutdown_rejects_new_job_registration_and_runs() {
        let mut scheduler = Scheduler::new();
        let run_count = Arc::new(AtomicUsize::new(0));
        let run_count_for_job = run_count.clone();

        let registration =
            scheduler.register_job(JobSpec::new("fake", Duration::from_secs(60), move |_| {
                let run_count = run_count_for_job.clone();
                async move {
                    run_count.fetch_add(1, Ordering::AcqRel);
                }
            }));
        assert!(registration.is_ok());

        scheduler.shutdown();

        let late_registration =
            scheduler.register_job(JobSpec::new("late", Duration::from_secs(60), |_| async {}));
        assert!(matches!(
            late_registration,
            Err(SchedulerRegisterError::ShuttingDown)
        ));

        let run_after_shutdown = scheduler.run_job_now("fake");
        assert!(matches!(run_after_shutdown, Err(JobRunError::ShuttingDown)));
        assert_eq!(run_count.load(Ordering::Acquire), 0);
    }
}

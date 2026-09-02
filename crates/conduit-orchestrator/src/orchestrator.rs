use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use crate::candidate::Candidate;
use crate::candidates::{CandidateRequest, ChannelModelsCandidate, SelectionDiagnostics};
use crate::load_balancer::{
    CostAwareScoring, LoadBalancerStrategy, RetryPolicy as LbRetryPolicy, ScoringStrategy,
    ScoringStrategySet, StaticStickyKeyProvider, StickyKeyProvider, resolve_strategy,
    select_channels_with_tie_rotation,
};
use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, HttpResponse, LlmRequest, StreamEvent};
use conduit_pipeline::middleware::PipelineContext;
use conduit_pipeline::pipeline::{
    AttemptRecord as PipelineAttempt, Pipeline, PipelineCandidate,
    RetryPolicy as PipelineRetryPolicy,
};
use conduit_services::RouteHealthStatus;
use conduit_services::{ExecutionRecord, RequestStatus};
use conduit_transformers::InboundTransformer;
use serde_json::{Value, json};
use thiserror::Error;

pub type OrchestratorResult<T> = Result<T, OrchestratorError>;

pub const STICKY_CHANNEL_ID_METADATA: &str = "sticky_channel_id";
pub const ROUTE_AFFINITY_HINTS_METADATA: &str = "route_affinity_hints";
pub const ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA: &str =
    "route_affinity_previous_response_hash";
pub const ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA: &str = "route_affinity_prompt_cache_hash";
pub const ROUTE_AFFINITY_PUBLIC_MODEL_METADATA: &str = "route_affinity_public_model";
pub const ROUTE_AFFINITY_API_FORMAT_METADATA: &str = "route_affinity_api_format";
pub const ROUTE_AFFINITY_APPLIED_CLASS_METADATA: &str = "route_affinity_applied_class";
pub const ROUTE_AFFINITY_KEY_CLASS_METADATA: &str = "route_affinity_key_class";
pub const ROUTE_AFFINITY_DECISION_METADATA: &str = "route_affinity_decision";

/// Sanitized successful-route feedback loaded by the host. All identity values
/// are one-way digests/fingerprints; this type never carries provider secrets.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct RouteAffinityHint {
    pub key_class: String,
    pub channel_id: String,
    pub upstream_model_id: String,
    pub upstream_api_format: String,
    pub credential_identity: Option<String>,
}

/// A credential-free key used by runtime health admission. The credential is
/// represented only by its stable one-way fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteHealthTarget {
    pub channel_id: String,
    pub actual_model: String,
    pub credential_identity: Option<String>,
}

#[async_trait]
pub trait RouteHealthSource: Send + Sync {
    async fn statuses(
        &self,
        targets: &[RouteHealthTarget],
    ) -> Result<BTreeMap<RouteHealthTarget, RouteHealthStatus>, ConduitError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRouteHealthSource;

#[async_trait]
impl RouteHealthSource for NoopRouteHealthSource {
    async fn statuses(
        &self,
        _targets: &[RouteHealthTarget],
    ) -> Result<BTreeMap<RouteHealthTarget, RouteHealthStatus>, ConduitError> {
        Ok(BTreeMap::new())
    }
}

/// Raw profile override stamped by API-key authentication.
const API_KEY_LOAD_BALANCE_STRATEGY_METADATA: &str = "api_key_load_balance_strategy";
/// Effective strategy after resolving `system_default`; pipeline middlewares
/// consume this key (not the raw profile value).
pub const LOAD_BALANCE_STRATEGY_METADATA: &str = "load_balance_strategy";

/// One settings snapshot shared by candidate ordering and pipeline execution
/// for a single request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeRetryPolicy {
    pub load_balancer: LbRetryPolicy,
    pub pipeline: PipelineRetryPolicy,
    pub cost_score_weight: i64,
}

#[async_trait]
pub trait RuntimeRetryPolicySource: Send + Sync {
    async fn current(&self) -> RuntimeRetryPolicy;
}

/// Stable, process-independent hash used only to rotate equal-priority routing
/// ties. FNV-1a avoids randomized `HashMap` state and keeps a trace/request on
/// the same channel across workers with the same candidate set.
fn stable_routing_offset(key: &str) -> usize {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorStage {
    Auth,
    Quota,
    Select,
    LoadBalance,
    Pipeline,
    Persist,
    Emit,
}

impl OrchestratorStage {
    pub const ALL: [Self; 7] = [
        Self::Auth,
        Self::Quota,
        Self::Select,
        Self::LoadBalance,
        Self::Pipeline,
        Self::Persist,
        Self::Emit,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Quota => "quota",
            Self::Select => "select",
            Self::LoadBalance => "load_balance",
            Self::Pipeline => "pipeline",
            Self::Persist => "persist",
            Self::Emit => "emit",
        }
    }
}

impl fmt::Display for OrchestratorStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestratorCommand {
    Http(HttpRequest),
    Llm(LlmRequest),
}

impl OrchestratorCommand {
    pub const fn wants_stream(&self) -> bool {
        match self {
            Self::Http(_) => false,
            Self::Llm(request) => request.stream,
        }
    }
}

impl From<HttpRequest> for OrchestratorCommand {
    fn from(request: HttpRequest) -> Self {
        Self::Http(request)
    }
}

impl From<LlmRequest> for OrchestratorCommand {
    fn from(request: LlmRequest) -> Self {
        Self::Llm(request)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OrchestratorStream {
    pub events: Vec<StreamEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrchestratorResponse {
    Http(HttpResponse),
    Stream(OrchestratorStream),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestratorContext {
    pub stages: Vec<OrchestratorStage>,
    pub metadata: BTreeMap<String, String>,
}

impl OrchestratorContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_stage(&mut self, stage: OrchestratorStage) {
        self.stages.push(stage);
    }
}

fn request_sticky_provider(ctx: &OrchestratorContext) -> Option<StaticStickyKeyProvider> {
    ctx.metadata
        .get(STICKY_CHANNEL_ID_METADATA)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .map(StaticStickyKeyProvider::fixed)
}

#[derive(Clone)]
struct ResolvedRouteAffinity {
    index: usize,
    key_class: String,
    credential: Option<String>,
}

fn request_route_affinity_hints(ctx: &OrchestratorContext) -> Vec<RouteAffinityHint> {
    ctx.metadata
        .get(ROUTE_AFFINITY_HINTS_METADATA)
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}

fn candidate_api_format(candidate: &ChannelModelsCandidate) -> &str {
    if candidate.endpoint.api_format.is_empty() {
        &candidate.api_format
    } else {
        &candidate.endpoint.api_format
    }
}

fn affinity_credential(
    candidate: &ChannelModelsCandidate,
    expected_identity: Option<&str>,
) -> Option<Option<String>> {
    let credentials: Vec<Option<String>> = if candidate.enabled_credentials.is_empty() {
        vec![candidate.active_credential.clone()]
    } else {
        candidate
            .enabled_credentials
            .iter()
            .map(|credential| Some(credential.clone()))
            .collect()
    };

    credentials.into_iter().find(|credential| {
        credential
            .as_deref()
            .map(conduit_services::credential_fingerprint)
            .as_deref()
            == expected_identity
    })
}

fn resolve_route_affinity(
    hints: &[RouteAffinityHint],
    resolved: &[ChannelModelsCandidate],
    ordered_indices: &[usize],
    health_statuses: &BTreeMap<RouteHealthTarget, RouteHealthStatus>,
) -> Option<ResolvedRouteAffinity> {
    for hint in hints {
        for index in ordered_indices {
            let Some(candidate) = resolved.get(*index) else {
                continue;
            };
            let actual_model = candidate
                .models
                .first()
                .map(|model| model.actual_model.as_str())
                .unwrap_or_default();
            if candidate.channel_id != hint.channel_id
                || actual_model != hint.upstream_model_id
                || candidate_api_format(candidate) != hint.upstream_api_format
            {
                continue;
            }
            let Some(credential) =
                affinity_credential(candidate, hint.credential_identity.as_deref())
            else {
                continue;
            };
            let target = RouteHealthTarget {
                channel_id: candidate.channel_id.clone(),
                actual_model: actual_model.to_string(),
                credential_identity: credential
                    .as_deref()
                    .map(conduit_services::credential_fingerprint),
            };
            if health_statuses.get(&target).copied() == Some(RouteHealthStatus::Unhealthy) {
                continue;
            }
            return Some(ResolvedRouteAffinity {
                index: *index,
                key_class: hint.key_class.clone(),
                credential,
            });
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
}

impl AttemptStatus {
    pub const fn as_request_status(self) -> RequestStatus {
        match self {
            Self::Running => RequestStatus::Running,
            Self::Succeeded => RequestStatus::Succeeded,
            Self::Failed => RequestStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRecord {
    pub id: String,
    pub request_id: String,
    pub project_id: String,
    pub sequence: u32,
    pub channel_id: String,
    pub provider: String,
    pub model: String,
    pub status: AttemptStatus,
}

impl AttemptRecord {
    pub fn for_candidate(
        request_id: impl Into<String>,
        project_id: impl Into<String>,
        sequence: u32,
        candidate: &Candidate,
    ) -> Self {
        let request_id = request_id.into();

        Self {
            id: format!("{request_id}-attempt-{sequence}"),
            request_id,
            project_id: project_id.into(),
            sequence,
            channel_id: candidate.id.clone(),
            provider: candidate.provider.clone(),
            model: candidate.model.clone(),
            status: AttemptStatus::Running,
        }
    }

    pub const fn succeeded(mut self) -> Self {
        self.status = AttemptStatus::Succeeded;
        self
    }

    pub const fn failed(mut self) -> Self {
        self.status = AttemptStatus::Failed;
        self
    }

    pub fn to_execution_record(&self) -> ExecutionRecord {
        let mut execution = ExecutionRecord::new(
            self.id.clone(),
            self.request_id.clone(),
            self.project_id.clone(),
            self.sequence,
        );
        execution.status = self.status.as_request_status();
        execution.provider = Some(self.provider.clone());
        execution.model = Some(self.model.clone());
        execution
            .extra
            .insert("channel_id".to_string(), json!(self.channel_id.as_str()));
        execution
    }
}

#[derive(Debug, Error)]
#[error("orchestrator failed at {failed_stage}: {source}")]
pub struct OrchestratorError {
    pub failed_stage: OrchestratorStage,
    #[source]
    pub source: ConduitError,
}

impl OrchestratorError {
    pub fn new(failed_stage: OrchestratorStage, source: ConduitError) -> Self {
        Self {
            failed_stage,
            source,
        }
    }
}

pub trait Orchestrator: Send + Sync {
    fn process(
        &self,
        ctx: &mut OrchestratorContext,
        command: OrchestratorCommand,
    ) -> OrchestratorResult<OrchestratorResponse>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkeletonOrchestrator {
    fail_at: Option<OrchestratorStage>,
}

impl SkeletonOrchestrator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn failing_at(stage: OrchestratorStage) -> Self {
        Self {
            fail_at: Some(stage),
        }
    }

    fn placeholder_response(command: &OrchestratorCommand) -> OrchestratorResponse {
        if command.wants_stream() {
            return OrchestratorResponse::Stream(OrchestratorStream::default());
        }

        OrchestratorResponse::Http(HttpResponse {
            status: 202,
            metadata: BTreeMap::from([(
                "orchestrator".to_string(),
                json!("skeleton_no_provider_call"),
            )]),
            ..HttpResponse::default()
        })
    }
}

impl Orchestrator for SkeletonOrchestrator {
    fn process(
        &self,
        ctx: &mut OrchestratorContext,
        command: OrchestratorCommand,
    ) -> OrchestratorResult<OrchestratorResponse> {
        for stage in OrchestratorStage::ALL {
            ctx.record_stage(stage);

            if self.fail_at == Some(stage) {
                return Err(OrchestratorError::new(
                    stage,
                    ConduitError::internal(format!(
                        "skeleton orchestrator stopped at {}",
                        stage.as_str()
                    )),
                ));
            }
        }

        Ok(Self::placeholder_response(&command))
    }
}

// ===========================================================================
// CommandOrchestrator (RUST-P9-006) — the real request-execution flow.
// ===========================================================================
//
// Wires together the components built in RUST-P9-003..005 + RUST-P8-002 into
// the Go `ChatCompletionOrchestrator.Process` command flow:
//
//   1. **Select**  (Go `selectCandidates` middleware) — resolve the candidate
//      channels for the request. Empty → `Select`-stage error (Go returns
//      `ErrInvalidModel`).
//   2. **LoadBalance** (Go `LoadBalancer.Sort`) — score + top-K + sticky
//      rotation, yielding the ordered attempt list.
//   3. **Pipeline** (Go `pipeline.Process`) — inbound transform → attempt loop
//      (outbound → merge → auth → execute) → outbound. Retry/failover is owned
//      by the pipeline; the orchestrator only hands it the ordered candidates.
//   4. **Record** (Go `persistRequestExecution` + `CreateUsageLogFromRequest`)
//      — persist the RequestExecution status and the usage log. On error the
//      Go code additionally calls `UpdateRequestExecutionStatusFromError` /
//      `UpdateRequestStatusFromError`.
//
// Each stage's failure is tagged with its `OrchestratorStage` so operators can
// see *where* a request died (mirrors the Go error-tagging discipline). The
// flow honors a cancellation signal: a canceled context short-circuits before
// the pipeline runs (the pipeline itself re-checks cancel between attempts).

// ---------------------------------------------------------------------------
// Trait stubs — the IO collaborators of the command flow.
// ---------------------------------------------------------------------------

/// Resolves the raw channel candidates for a request (Go `CandidateSelector.Select`).
/// Pure-logic implementations live in [`crate::candidates`]; production wires a
/// DB-backed selector. Returns the candidate list (possibly empty — the
/// orchestrator turns an empty list into a `Select`-stage error).
#[async_trait]
pub trait CandidateSource: Send + Sync {
    async fn select(
        &self,
        request: &CandidateRequest,
    ) -> Result<Vec<ChannelModelsCandidate>, ConduitError>;

    /// Selection plus safe, credential-free diagnostics. Implementations that
    /// have no staged filter information still expose the final survivors.
    async fn select_with_diagnostics(
        &self,
        request: &CandidateRequest,
    ) -> Result<(Vec<ChannelModelsCandidate>, SelectionDiagnostics), ConduitError> {
        let candidates = self.select(request).await?;
        let selected = candidates
            .iter()
            .map(|candidate| crate::candidates::SelectedCandidateRef {
                channel_id: candidate.channel_id.clone(),
                channel_name: candidate.channel_name.clone(),
                priority: candidate.priority,
                api_format: candidate.api_format.clone(),
            })
            .collect();
        Ok((
            candidates,
            SelectionDiagnostics {
                selected,
                rejected: Vec::new(),
            },
        ))
    }
}

/// Converts a resolved [`ChannelModelsCandidate`] list into the
/// load-balancer-facing [`Candidate`] view (Go flattens candidates into the LB's
/// `*ChannelModelsCandidate` then sorts them). Kept as a trait so the
/// orchestrator does not depend on the candidate-projection details.
pub trait CandidateProjector: Send + Sync {
    fn project(&self, resolved: &[ChannelModelsCandidate]) -> Vec<Candidate>;
}

/// Metadata key carrying the stable request-scoped billing idempotency key.
///
/// It is written before the asynchronous reservation call starts so a dropped
/// admission future can still release a reservation that committed just before
/// cancellation. Reservation implementations must use
/// [`BillingAdmissionInput::request_key`] as their stable request identity.
pub const BILLING_ADMISSION_REQUEST_KEY_METADATA: &str = "billing_admission_request_key";

/// Persists request-execution state and usage logs (Go `RequestService` +
/// `UsageLogService`). The orchestrator calls these on the success and error
/// paths of the pipeline. Errors here are surfaced with the `Persist` stage tag
/// but never mask the original pipeline outcome (mirrors Go's `log.Warn` +
/// `return err`).
#[async_trait]
pub trait RequestRecorder: Send + Sync {
    /// Perform request-level admission as one lifecycle operation. Keeping the
    /// concurrency acquire and wallet reserve together guarantees that a
    /// failed reserve cannot leak the API-key slot acquired immediately before
    /// it. Retries never call this method again.
    async fn admit_request(
        &self,
        ctx: &mut OrchestratorContext,
        input: &BillingAdmissionInput,
        api_key_limit: Option<u32>,
    ) -> Result<(), ConduitError> {
        ctx.metadata.insert(
            BILLING_ADMISSION_REQUEST_KEY_METADATA.to_owned(),
            input.request_key.clone(),
        );
        if let (Some(api_key_id), Some(limit)) = (
            input
                .api_key_id
                .as_deref()
                .and_then(|value| value.parse::<i64>().ok()),
            api_key_limit.filter(|limit| *limit > 0),
        ) {
            self.acquire_api_key_slot(ctx, api_key_id, limit)?;
        }

        // `reserve_request` may await PostgreSQL after the in-memory
        // concurrency slot has already been acquired. An outer timeout drops
        // this future; ordinary control-flow cleanup would never run. The
        // borrowed guard closes that cancellation window. On success,
        // ownership moves to the request-level guard installed by the command
        // flow before its next await point.
        let mut cleanup =
            BorrowedAdmissionGuard::new(self, ctx.clone(), "request admission interrupted");
        self.reserve_request(ctx, input).await?;
        cleanup.disarm();
        Ok(())
    }

    /// Acquire a request-scoped API-key concurrency slot. The slot is shared
    /// across retries and released only when the whole request/stream ends.
    fn acquire_api_key_slot(
        &self,
        _ctx: &mut OrchestratorContext,
        _api_key_id: i64,
        _limit: u32,
    ) -> Result<(), ConduitError> {
        Ok(())
    }

    fn release_api_key_slot(&self, _ctx: &OrchestratorContext) {}

    /// Finalize an admitted request whose future was dropped before a normal
    /// success/failure recorder callback could finish.
    ///
    /// This method is invoked from `Drop`: implementations must be
    /// non-blocking, panic-free, and idempotent. Synchronous resources should
    /// be released immediately; asynchronous durable cleanup may be delegated
    /// to a supervised task and backed by expiry/reconciliation.
    fn abandon_request(&self, ctx: &OrchestratorContext, _reason: &'static str) {
        self.release_api_key_slot(ctx);
    }

    /// Reserve the estimated maximum customer charge before any upstream
    /// attempt starts. Implementations that do not own a wallet keep the
    /// default no-op behavior.
    async fn reserve_request(
        &self,
        _ctx: &mut OrchestratorContext,
        _input: &BillingAdmissionInput,
    ) -> Result<(), ConduitError> {
        Ok(())
    }

    /// Record a successful attempt's execution + usage (Go
    /// `persistRequestExecution` + `CreateUsageLogFromRequest`).
    async fn record_success(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        attempt: &PipelineAttempt,
        response: &HttpResponse,
    ) -> Result<(), ConduitError>;

    /// Update execution/request status from an error (Go
    /// `UpdateRequestExecutionStatusFromError` / `UpdateRequestStatusFromError`).
    async fn record_failure(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        error: &ConduitError,
    ) -> Result<(), ConduitError>;

    /// Consume a [`StreamFinalPlan`] and persist the streaming attempt's
    /// execution + chunks + usage (Go `OutboundPersistentStream.Close` ->
    /// `persistAggregatedResponse` / `SaveRequestExecutionChunks` /
    /// `UpdateRequestExecutionStatusFromError`, outbound.go:100-305).
    ///
    /// # Inputs
    ///
    /// * `plan`            — the pure decision ([`stream_final_plan`]).
    /// * `execution_id`    — the per-attempt execution row id (Go
    ///   `ts.requestExec.ID`). `None` mirrors Go's `ts.requestExec == nil` guard
    ///   (recorder must no-op).
    /// * `aggregated`      — the aggregated [`HttpResponse`] from the
    ///   [`crate::outbound_stream::StreamChunkAggregator`] (Go
    ///   `ts.transformer.AggregateStreamChunks`). Only consulted when
    ///   [`StreamFinalPlan::write_chunks`] is true; `None` is valid on the
    ///   failure branches.
    /// * `chunks`          — the buffered [`StreamEvent`]s the wrapper observed
    ///   (Go `ts.responseChunks`). Forwarded to
    ///   [`conduit_services::RequestService::save_request_execution_chunks`]
    ///   when `write_chunks` is true. The caller is responsible for
    ///   binary-summarizing them (Go `SummarizeBinaryChunk`).
    ///
    /// Default no-op so [`NoopRequestRecorder`] and any test stubs stay simple;
    /// [`crate::request_recorder::ProductionRequestRecorder`] overrides it.
    async fn record_stream_final(
        &self,
        _ctx: &OrchestratorContext,
        _request_id: &str,
        _project_id: &str,
        _plan: &StreamFinalPlan,
        _execution_id: Option<&str>,
        _aggregated: Option<&HttpResponse>,
        _chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        Ok(())
    }

    /// S38 — persist the **request-level** chunk array after a completed
    /// stream (Go `InboundPersistentStream._persistResponse` →
    /// `SaveRequestChunks`, `inbound.go:253-256`). Distinct from the
    /// execution-level chunks written by [`Self::record_stream_final`] (Go
    /// `SaveRequestExecutionChunks`, outbound.go:302). Failures are non-fatal
    /// in Go (`log.Warn`); implementations mirror that. Default no-op keeps
    /// stubs simple.
    async fn record_stream_request_chunks(
        &self,
        _ctx: &OrchestratorContext,
        _request_id: &str,
        _project_id: &str,
        _chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        Ok(())
    }
}

/// Covers cancellation while `RequestRecorder::admit_request` is awaiting its
/// durable reservation. This guard borrows the recorder because admission is a
/// trait default method and therefore does not own the surrounding `Arc`.
struct BorrowedAdmissionGuard<'a, R: RequestRecorder + ?Sized> {
    recorder: &'a R,
    ctx: OrchestratorContext,
    reason: &'static str,
    armed: bool,
}

impl<'a, R: RequestRecorder + ?Sized> BorrowedAdmissionGuard<'a, R> {
    fn new(recorder: &'a R, ctx: OrchestratorContext, reason: &'static str) -> Self {
        Self {
            recorder,
            ctx,
            reason,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl<R: RequestRecorder + ?Sized> Drop for BorrowedAdmissionGuard<'_, R> {
    fn drop(&mut self) {
        if self.armed {
            self.recorder.abandon_request(&self.ctx, self.reason);
        }
    }
}

/// Owns request admission after the reservation call completes. Because it
/// owns both the recorder and a metadata snapshot, dropping any outer handler,
/// timeout, or pipeline future still reaches the abandonment hook.
struct RequestAdmissionGuard {
    recorder: Arc<dyn RequestRecorder>,
    ctx: OrchestratorContext,
    reason: &'static str,
    armed: bool,
}

impl RequestAdmissionGuard {
    fn new(
        recorder: Arc<dyn RequestRecorder>,
        ctx: &OrchestratorContext,
        reason: &'static str,
    ) -> Self {
        Self {
            recorder,
            ctx: ctx.clone(),
            reason,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RequestAdmissionGuard {
    fn drop(&mut self) {
        if self.armed {
            self.recorder.abandon_request(&self.ctx, self.reason);
        }
    }
}

/// Request-level wallet admission input. The reservation belongs to the
/// customer request, not an upstream attempt, so retries and channel failover
/// share one idempotency key and can never reserve twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingAdmissionInput {
    pub request_key: String,
    pub project_id: String,
    pub api_key_id: Option<String>,
    pub public_model: String,
    pub estimated_input_tokens: u64,
    pub max_output_tokens: u64,
}

fn estimate_candidate_input_tokens(request: &CandidateRequest) -> u64 {
    // This is intentionally conservative and tokenizer-independent. Four UTF-8
    // bytes per token is a common approximation; message/tool framing gets a
    // fixed allowance so short structured requests are not underestimated.
    let bytes = format!("{:?}{:?}", request.messages, request.tools).len() as u64;
    bytes
        .div_ceil(4)
        .saturating_add((request.messages.len() as u64).saturating_mul(8))
        .max(1)
}

/// A no-op recorder for tests and the skeleton path — records nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRequestRecorder;

#[async_trait]
impl RequestRecorder for NoopRequestRecorder {
    async fn record_success(
        &self,
        _ctx: &OrchestratorContext,
        _request_id: &str,
        _project_id: &str,
        _attempt: &PipelineAttempt,
        _response: &HttpResponse,
    ) -> Result<(), ConduitError> {
        Ok(())
    }

    async fn record_failure(
        &self,
        _ctx: &OrchestratorContext,
        _request_id: &str,
        _project_id: &str,
        _error: &ConduitError,
    ) -> Result<(), ConduitError> {
        Ok(())
    }
}

/// A cancellation signal the orchestrator checks before the pipeline runs and
/// that the pipeline re-checks between attempts. Production wires a real token
/// (Go `ctx.Done()`); tests inject a flag-backed stub.
pub trait CancelToken: Send + Sync {
    fn is_canceled(&self) -> bool;
}

/// A flag-backed cancel token for tests.
#[derive(Debug, Default)]
pub struct FlagCancelToken {
    canceled: std::sync::Mutex<bool>,
}

impl FlagCancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        if let Ok(mut g) = self.canceled.lock() {
            *g = true;
        }
    }
}

impl CancelToken for FlagCancelToken {
    fn is_canceled(&self) -> bool {
        self.canceled.lock().map(|g| *g).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Default projection — flatten resolved candidates into LB Candidates.
// ---------------------------------------------------------------------------

/// Default [`CandidateProjector`]: one [`Candidate`] per resolved candidate,
/// preserving channel id / provider / model. The LB-side `ordering_weight` is
/// taken from the channel's own `Channel.OrderingWeight`; association
/// `priority` is a distinct concept and must not leak into channel scoring.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultCandidateProjector;

impl CandidateProjector for DefaultCandidateProjector {
    fn project(&self, resolved: &[ChannelModelsCandidate]) -> Vec<Candidate> {
        resolved
            .iter()
            .filter_map(|c| {
                let model = c.models.first()?.actual_model.clone();
                Some(
                    Candidate::new(
                        c.channel_id.clone(),
                        c.channel_name.clone(),
                        model,
                        crate::candidate::CandidateStatus::Ready,
                    )
                    .with_ordering_weight(c.ordering_weight)
                    .with_routing_cost(
                        c.theoretical_cost_accounting.clone(),
                        c.cost_efficiency_score,
                    ),
                )
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// CommandOrchestrator — owns the collaborators and runs the 4-stage flow.
// ---------------------------------------------------------------------------

/// The real request-execution orchestrator. Composes the candidate source, load
/// balancer (scoring strategy + sticky provider + retry policy), the pipeline
/// (transformers + executor + retry hooks), and the request recorder into the
/// Go command flow.
///
/// Held together by trait objects so the pure-logic stages are unit-testable
/// with in-memory stubs.
pub struct CommandOrchestrator {
    candidate_source: Arc<dyn CandidateSource>,
    candidate_projector: Arc<dyn CandidateProjector>,
    scoring_strategies: ScoringStrategySet,
    sticky_provider: Arc<dyn StickyKeyProvider>,
    retry_policy: LbRetryPolicy,
    runtime_retry_policy_source: Option<Arc<dyn RuntimeRetryPolicySource>>,
    pipeline: Arc<Pipeline>,
    recorder: Arc<dyn RequestRecorder>,
    cancel_token: Arc<dyn CancelToken>,
    route_health: Arc<dyn RouteHealthSource>,
}

impl CommandOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        candidate_source: Arc<dyn CandidateSource>,
        candidate_projector: Arc<dyn CandidateProjector>,
        scoring_strategy: Arc<dyn ScoringStrategy>,
        sticky_provider: Arc<dyn StickyKeyProvider>,
        retry_policy: LbRetryPolicy,
        pipeline: Arc<Pipeline>,
        recorder: Arc<dyn RequestRecorder>,
        cancel_token: Arc<dyn CancelToken>,
    ) -> Self {
        Self {
            candidate_source,
            candidate_projector,
            scoring_strategies: ScoringStrategySet::uniform(scoring_strategy),
            sticky_provider,
            retry_policy,
            runtime_retry_policy_source: None,
            pipeline,
            recorder,
            cancel_token,
            route_health: Arc::new(NoopRouteHealthSource),
        }
    }

    /// Install the three long-lived load-balancer implementations selected by
    /// the effective system/API-key strategy on each request. `new` remains
    /// backward compatible by applying its single scorer to all three modes.
    pub fn with_scoring_strategies(mut self, scoring_strategies: ScoringStrategySet) -> Self {
        self.scoring_strategies = scoring_strategies;
        self
    }

    pub fn with_route_health_source(mut self, source: Arc<dyn RouteHealthSource>) -> Self {
        self.route_health = source;
        self
    }

    pub fn with_runtime_retry_policy_source(
        mut self,
        source: Arc<dyn RuntimeRetryPolicySource>,
    ) -> Self {
        self.runtime_retry_policy_source = Some(source);
        self
    }

    async fn runtime_retry_policy(&self) -> Option<RuntimeRetryPolicy> {
        match self.runtime_retry_policy_source.as_ref() {
            Some(source) => Some(source.current().await),
            None => None,
        }
    }

    fn request_load_balance_strategy(
        &self,
        request: &HttpRequest,
        retry_policy: LbRetryPolicy,
    ) -> LoadBalancerStrategy {
        let profile_override = request
            .metadata
            .get(API_KEY_LOAD_BALANCE_STRATEGY_METADATA)
            .and_then(Value::as_str);
        resolve_strategy(retry_policy.strategy.as_str(), profile_override)
    }

    /// Expose the candidate source for the bridge layer (needed by
    /// `resolve_candidates` which runs the inbound transformer + selection
    /// before delegating to the full orchestrator flow).
    pub fn candidate_source(&self) -> &dyn CandidateSource {
        self.candidate_source.as_ref()
    }

    /// Run the command flow for an inbound HTTP request (Go
    /// `ChatCompletionOrchestrator.Process`), using the pipeline's configured
    /// default inbound transformer. Convenience wrapper over
    /// [`Self::process_command_with_inbound`] — kept so existing callers/tests
    /// that do not resolve a per-route inbound are unchanged.
    ///
    /// Stage order (each tagged on `ctx.stages` for operability):
    /// `Select` → `LoadBalance` → `Pipeline` → `Persist`.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_command(
        &self,
        ctx: &mut OrchestratorContext,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> OrchestratorResult<HttpResponse> {
        self.process_command_with_inbound(
            ctx,
            self.pipeline.inbound(),
            request_id,
            project_id,
            candidate_request,
            http_request,
            raw_inbound,
            trace_id,
            thread_id,
            // No external cancel for this convenience shape — callers that need
            // client-disconnect cancellation use `process_command_with_inbound`
            // directly (the bridge does) and pass a per-request token (P-09).
            None,
        )
        .await
    }

    /// Run the command flow for an inbound HTTP request (Go
    /// `ChatCompletionOrchestrator.Process`). Returns the final HTTP response
    /// and the per-attempt records (for observability / usage recording).
    ///
    /// `inbound` is the client's wire-format transformer for THIS request,
    /// resolved from the route by the bridge (Go binds a per-format inbound into
    /// a dedicated orchestrator — e.g. `anthropic.go:45-59`). It is threaded into
    /// [`Pipeline::process_with_inbound`] so the client body is parsed and the
    /// client-facing response is reshaped in the caller's own format (Anthropic,
    /// Gemini, …) rather than the pipeline's fixed default inbound.
    ///
    /// Stage order (each tagged on `ctx.stages` for operability):
    /// `Select` → `LoadBalance` → `Pipeline` → `Persist`.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_command_with_inbound(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: &dyn InboundTransformer,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        external_cancel: Option<conduit_pipeline::CancelToken>,
    ) -> OrchestratorResult<HttpResponse> {
        self.process_command_with_inbound_impl(
            ctx,
            inbound,
            request_id,
            project_id,
            candidate_request,
            http_request,
            raw_inbound,
            trace_id,
            thread_id,
            external_cancel,
            None,
        )
        .await
    }

    /// Run the buffered command flow using candidates already resolved by the
    /// caller. The HTTP bridge uses this after its route-specific inbound
    /// transform, avoiding a second candidate-source query on the same request.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_command_with_resolved_candidates(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: &dyn InboundTransformer,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        external_cancel: Option<conduit_pipeline::CancelToken>,
        resolved_candidates: &[ChannelModelsCandidate],
    ) -> OrchestratorResult<HttpResponse> {
        self.process_command_with_inbound_impl(
            ctx,
            inbound,
            request_id,
            project_id,
            candidate_request,
            http_request,
            raw_inbound,
            trace_id,
            thread_id,
            external_cancel,
            Some(resolved_candidates),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_command_with_inbound_impl(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: &dyn InboundTransformer,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        external_cancel: Option<conduit_pipeline::CancelToken>,
        resolved_candidates: Option<&[ChannelModelsCandidate]>,
    ) -> OrchestratorResult<HttpResponse> {
        // ---- Client disconnect check (Go `ctx.Err()` at the top of Process) ----
        if self.cancel_token.is_canceled() {
            ctx.record_stage(OrchestratorStage::Pipeline);
            return Err(OrchestratorError::new(
                OrchestratorStage::Pipeline,
                ConduitError::internal("request canceled before pipeline"),
            ));
        }

        // ---- S01 Select + S02 LoadBalance (shared with process_command_stream) ----
        // Resolve candidates, project → score → top-K → sticky, then look each
        // ordered id back up in `resolved` to recover the full channel target
        // (base_url / credential / actual model / api format) the pipeline
        // stamps per attempt. Extracted so the streaming path reuses the exact
        // same selection + security-sensitive credential threading.
        let runtime_retry_policy = self.runtime_retry_policy().await;
        let load_balancer_retry_policy = runtime_retry_policy
            .map(|policy| policy.load_balancer)
            .unwrap_or(self.retry_policy);
        let load_balance_strategy =
            self.request_load_balance_strategy(&http_request, load_balancer_retry_policy);
        ctx.metadata.insert(
            LOAD_BALANCE_STRATEGY_METADATA.to_string(),
            load_balance_strategy.as_str().to_string(),
        );
        let ordered_candidates = self
            .resolve_ordered_candidates(
                ctx,
                request_id,
                candidate_request,
                load_balance_strategy,
                load_balancer_retry_policy,
                runtime_retry_policy.map(|policy| policy.cost_score_weight),
                trace_id,
                thread_id,
                resolved_candidates,
            )
            .await?;

        // Wallet admission is request-scoped and deliberately runs after
        // access-aware candidate selection but before the pipeline performs
        // its first upstream attempt.
        let admission = BillingAdmissionInput {
            request_key: request_id.to_string(),
            project_id: project_id.to_string(),
            api_key_id: http_request
                .metadata
                .get("api_key_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    http_request
                        .metadata
                        .get("api_key_id")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                }),
            public_model: candidate_request.model.clone(),
            estimated_input_tokens: estimate_candidate_input_tokens(candidate_request),
            max_output_tokens: candidate_request
                .max_output_tokens
                .map(u64::from)
                .unwrap_or(4096),
        };
        let api_key_limit = http_request
            .metadata
            .get("api_key_max_concurrent")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        self.recorder
            .admit_request(ctx, &admission, api_key_limit)
            .await
            .map_err(|error| OrchestratorError::new(OrchestratorStage::Quota, error))?;
        let mut admission_guard = RequestAdmissionGuard::new(
            Arc::clone(&self.recorder),
            ctx,
            "buffered request future dropped before terminal recording",
        );

        // ---- S03 Pipeline: inbound → attempt loop → outbound → execute ----
        // The pipeline owns retry/failover (RUST-P8-002); orchestrator re-checks
        // cancellation between the pipeline's attempts via the cancel token.
        let mut pipeline_ctx = PipelineContext::new();
        // P-09: wire the caller's per-request cancel token (fired by the HTTP
        // layer's drop-guard on client disconnect) into the pipeline context so
        // the between-attempt cancel check (`is_context_canceled`,
        // `pipeline.rs:1083`) stops retrying + billing the moment the client
        // goes away. Without this the buffered path minted a fresh, unwired
        // token and kept running the upstream to completion after a disconnect.
        if let Some(token) = external_cancel {
            pipeline_ctx.cancel = token;
        }

        // Copy string-valued entries from HttpRequest.metadata into
        // PipelineContext.metadata so pipeline middlewares can read API key
        // info (model whitelist, profile name, project id) without needing
        // PersistenceState. Mirrors Go's context-value plumbing where
        // `contexts.WithAPIKey(ctx, apiKey)` makes the key info available
        // throughout the pipeline via `contexts.GetAPIKey(ctx)`.
        //
        // Also mirror the identity keys onto the OrchestratorContext: the
        // usage-log recorder runs off `ctx` (OrchestratorContext), not the
        // pipeline_ctx, so without this it could never attribute a usage_log to
        // its API key (P-44: `api_key_id` was hard-coded to None, making every
        // non-RPM API-key quota count NULL rows and therefore never fire).
        for (key, value) in &http_request.metadata {
            let as_string = match value {
                serde_json::Value::String(s) => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            };
            if let Some(s) = as_string {
                pipeline_ctx.metadata.insert(key.clone(), s.clone());
                // Recorder-relevant identity keys also go on the orchestrator
                // context so `record_success`/`record_failure` can read them.
                if matches!(
                    key.as_str(),
                    "api_key_id" | "trace_id" | "client_ip" | "data_storage_id"
                ) {
                    ctx.metadata.insert(key.clone(), s);
                }
            }
        }
        pipeline_ctx.metadata.insert(
            LOAD_BALANCE_STRATEGY_METADATA.to_string(),
            load_balance_strategy.as_str().to_string(),
        );

        let outcome = match runtime_retry_policy {
            Some(policy) => {
                self.pipeline
                    .process_with_inbound_policy(
                        &mut pipeline_ctx,
                        inbound,
                        http_request,
                        raw_inbound,
                        &ordered_candidates,
                        policy.pipeline,
                    )
                    .await
            }
            None => {
                self.pipeline
                    .process_with_inbound(
                        &mut pipeline_ctx,
                        inbound,
                        http_request,
                        raw_inbound,
                        &ordered_candidates,
                    )
                    .await
            }
        };

        // Surface the pipeline order into the orchestrator context for
        // operability (debug builds / observability).
        ctx.metadata
            .insert("pipeline_steps".to_string(), pipeline_ctx.order.join(","));
        for key in [
            "actual_model",
            "request_model",
            "channel_id",
            "channel_type",
            "credential_identity",
            "format",
        ] {
            if let Some(value) = pipeline_ctx.metadata.get(key) {
                ctx.metadata.insert(key.to_string(), value.clone());
            }
        }

        // The HTTP bridge may not have a request id before persistence. The
        // persist middleware creates the canonical numeric DB row id during
        // the pipeline and publishes it on the context; usage logs must point
        // at that id rather than the bridge's empty pre-persist placeholder.
        let persisted_request_id = pipeline_ctx
            .metadata
            .get("__persist_request_id")
            .cloned()
            .or_else(|| pipeline_ctx.request_id.clone())
            .unwrap_or_else(|| request_id.to_string());

        let (response, attempts) = match outcome {
            Ok(value) => value,
            Err(err) => {
                ctx.record_stage(OrchestratorStage::Persist);
                // Go: update request/execution status from error before returning.
                if let Err(record_err) = self
                    .recorder
                    .record_failure(ctx, &persisted_request_id, project_id, &err)
                    .await
                {
                    // Recording failures never mask the original error (Go logs
                    // a warning). We still tag the stage for operability.
                    return Err(OrchestratorError::new(
                        OrchestratorStage::Persist,
                        record_err,
                    ));
                }
                admission_guard.disarm();
                return Err(OrchestratorError::new(OrchestratorStage::Pipeline, err));
            }
        };

        // ---- S04 Persist: record execution + usage (Go persistRequestExecution) ----
        ctx.record_stage(OrchestratorStage::Persist);
        if let Some(last_attempt) = attempts.last() {
            if let Err(record_err) = self
                .recorder
                .record_success(
                    ctx,
                    &persisted_request_id,
                    project_id,
                    last_attempt,
                    &response,
                )
                .await
            {
                return Err(OrchestratorError::new(
                    OrchestratorStage::Persist,
                    record_err,
                ));
            }
            admission_guard.disarm();
        }

        Ok(response)
    }

    /// Shared S01 Select + S02 LoadBalance prefix for both the buffered
    /// ([`Self::process_command`]) and live-stream ([`Self::process_command_stream`])
    /// flows: resolve candidates, project → score → top-K → sticky, then rebuild
    /// the ordered [`PipelineCandidate`] list (recovering base_url / credential /
    /// actual model / api format from `resolved`).
    ///
    /// ⚠ `credential` is a plaintext secret: never log or `{:?}`-format these
    /// candidates.
    async fn resolve_ordered_candidates(
        &self,
        ctx: &mut OrchestratorContext,
        request_id: &str,
        candidate_request: &CandidateRequest,
        load_balance_strategy: LoadBalancerStrategy,
        retry_policy: LbRetryPolicy,
        cost_score_weight: Option<i64>,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        pre_resolved: Option<&[ChannelModelsCandidate]>,
    ) -> OrchestratorResult<Vec<PipelineCandidate>> {
        // ---- S01 Select: resolve candidates (Go selectCandidates middleware) ----
        ctx.record_stage(OrchestratorStage::Select);
        let selected;
        let resolved = if let Some(resolved) = pre_resolved {
            resolved
        } else {
            selected = self
                .candidate_source
                .select(candidate_request)
                .await
                .map_err(|err| OrchestratorError::new(OrchestratorStage::Select, err))?;
            &selected
        };

        if resolved.is_empty() {
            // Go returns `ErrInvalidModel` (or quota-exhausted) when no
            // candidate survives selection.
            return Err(OrchestratorError::new(
                OrchestratorStage::Select,
                ConduitError::not_found(format!(
                    "no candidates available for model {:?}",
                    candidate_request.model
                )),
            ));
        }

        // ---- S02 LoadBalance: project → score → top-K → sticky (Go LB.Sort) ----
        ctx.record_stage(OrchestratorStage::LoadBalance);
        let routing_key = trace_id.or(thread_id).unwrap_or(request_id);
        let offset = stable_routing_offset(routing_key);
        // Health admission runs after the concrete credential is selected, so
        // retain the full LB order here. Truncating to top_k before health
        // filtering would hide a lower-ranked healthy fallback.
        let required_count = resolved.len();
        let priorities: BTreeSet<i64> = resolved
            .iter()
            .map(|candidate| candidate.priority)
            .collect();
        let mut ordered_indices = Vec::with_capacity(required_count);
        let request_sticky_provider = request_sticky_provider(ctx);
        let sticky_provider: &dyn StickyKeyProvider = match request_sticky_provider.as_ref() {
            Some(provider) => provider,
            None => self.sticky_provider.as_ref(),
        };
        let mut scoring_strategy = self.scoring_strategies.get_arc(load_balance_strategy);
        if let Some(weight) = cost_score_weight.filter(|weight| *weight != 0) {
            scoring_strategy = Arc::new(CostAwareScoring::new(scoring_strategy, weight));
        }

        // Model-association priority and channel load balancing are distinct
        // layers in Go: lower association priority groups are exhausted first,
        // and the selected LB strategy orders channels only *within* a group.
        // Applying one global score would let a high channel weight jump across
        // an administrator's model-association priority boundary.
        for priority in priorities {
            if ordered_indices.len() >= required_count {
                break;
            }
            let group: Vec<ChannelModelsCandidate> = resolved
                .iter()
                .filter(|candidate| candidate.priority == priority)
                .cloned()
                .collect();
            let projected = self.candidate_projector.project(&group);
            let ordered = select_channels_with_tie_rotation(
                &projected,
                scoring_strategy.as_ref(),
                retry_policy,
                sticky_provider,
                trace_id,
                thread_id,
                offset,
            );
            for candidate in ordered {
                let Some((index, _)) = resolved.iter().enumerate().find(|(index, original)| {
                    original.priority == priority
                        && original.channel_id == candidate.id
                        && !ordered_indices.contains(index)
                }) else {
                    continue;
                };
                ordered_indices.push(index);
                if ordered_indices.len() >= required_count {
                    break;
                }
            }
        }

        if ordered_indices.is_empty() {
            // All candidates projected to zero weight / filtered out.
            return Err(OrchestratorError::new(
                OrchestratorStage::LoadBalance,
                ConduitError::internal("load balancer selected no channel"),
            ));
        }

        let mut health_targets = Vec::new();
        for index in &ordered_indices {
            let Some(candidate) = resolved.get(*index) else {
                continue;
            };
            let actual_model = candidate
                .models
                .first()
                .map(|model| model.actual_model.clone())
                .unwrap_or_default();
            let credentials = if candidate.enabled_credentials.is_empty() {
                vec![candidate.active_credential.clone()]
            } else {
                candidate
                    .enabled_credentials
                    .iter()
                    .map(|value| Some(value.clone()))
                    .collect()
            };
            for credential in credentials {
                health_targets.push(RouteHealthTarget {
                    channel_id: candidate.channel_id.clone(),
                    actual_model: actual_model.clone(),
                    credential_identity: credential
                        .as_deref()
                        .map(conduit_services::credential_fingerprint),
                });
            }
        }
        let health_statuses = self
            .route_health
            .statuses(&health_targets)
            .await
            .map_err(|error| OrchestratorError::new(OrchestratorStage::LoadBalance, error))?;
        let affinity_hints = request_route_affinity_hints(ctx);
        let resolved_affinity = resolve_route_affinity(
            &affinity_hints,
            resolved,
            &ordered_indices,
            &health_statuses,
        );
        if let Some(affinity) = resolved_affinity.as_ref() {
            if let Some(position) = ordered_indices
                .iter()
                .position(|index| *index == affinity.index)
            {
                let index = ordered_indices.remove(position);
                ordered_indices.insert(0, index);
            }
            ctx.metadata.insert(
                ROUTE_AFFINITY_APPLIED_CLASS_METADATA.to_string(),
                affinity.key_class.clone(),
            );
            ctx.metadata.insert(
                ROUTE_AFFINITY_DECISION_METADATA.to_string(),
                "applied".to_string(),
            );
        } else if !affinity_hints.is_empty() {
            ctx.metadata.insert(
                ROUTE_AFFINITY_DECISION_METADATA.to_string(),
                "stale_or_ineligible".to_string(),
            );
        }
        let mut health_excluded = Vec::new();
        let mut healthy_ordered_indices = Vec::with_capacity(ordered_indices.len());
        let mut selected_credentials: BTreeMap<usize, Option<String>> = BTreeMap::new();
        for index in &ordered_indices {
            let Some(candidate) = resolved.get(*index) else {
                continue;
            };
            let actual_model = candidate
                .models
                .first()
                .map(|model| model.actual_model.clone())
                .unwrap_or_default();
            let selected_credential = resolved_affinity
                .as_ref()
                .filter(|affinity| affinity.index == *index)
                .map(|affinity| affinity.credential.clone())
                .or_else(|| {
                    select_healthy_credential(
                        &candidate.enabled_credentials,
                        candidate.active_credential.as_deref(),
                        trace_id,
                        &candidate.channel_id,
                        &actual_model,
                        &health_statuses,
                    )
                });
            if let Some(credential) = selected_credential {
                healthy_ordered_indices.push(*index);
                selected_credentials.insert(*index, credential);
            } else {
                health_excluded.push((
                    candidate.channel_id.clone(),
                    candidate.channel_name.clone(),
                    actual_model,
                ));
            }
        }
        if healthy_ordered_indices.is_empty() {
            return Err(OrchestratorError::new(
                OrchestratorStage::LoadBalance,
                ConduitError::upstream("all selected route targets are unhealthy"),
            ));
        }
        healthy_ordered_indices.truncate(retry_policy.top_k());

        if !health_excluded.is_empty() {
            let mut diagnostics: SelectionDiagnostics = ctx
                .metadata
                .get("route_selection_diagnostics")
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default();
            for (channel_id, channel_name, actual_model) in health_excluded {
                diagnostics
                    .rejected
                    .push(crate::candidates::SelectionRejection {
                        stage: crate::candidates::FilterStage::RouteHealth,
                        channel_id,
                        channel_name,
                        detail: format!(
                            "recent health marked actual model {actual_model} unhealthy"
                        ),
                    });
            }
            diagnostics.selected.retain(|selected| {
                healthy_ordered_indices.iter().any(|index| {
                    resolved
                        .get(*index)
                        .is_some_and(|candidate| candidate.channel_id == selected.channel_id)
                })
            });
            if let Ok(value) = serde_json::to_string(&diagnostics) {
                ctx.metadata
                    .insert("route_selection_diagnostics".to_owned(), value);
            }
        }

        // WIRE-06: rebuild the pipeline targets in LB order (see the buffered
        // path's comment for the full rationale). Every ordered id exists in
        // `resolved`; `actual_model` takes `models[0]`.
        let ordered_candidates: Vec<PipelineCandidate> = healthy_ordered_indices
            .iter()
            .filter_map(|index| resolved.get(*index).map(|candidate| (*index, candidate)))
            .map(|(index, r)| {
                // P-17: pick the per-request key from the full enabled set using
                // the request trace id (trace-sticky load balancing), instead of
                // the snapshot's no-trace `enabled[0]`. Falls back to the
                // pre-resolved `active_credential` when no multi-key set exists
                // (single key / OAuth / Azure / GCP channels).
                let credential = selected_credentials
                    .get(&index)
                    .cloned()
                    .unwrap_or_else(|| {
                        select_trace_sticky_credential(
                            &r.enabled_credentials,
                            r.active_credential.as_deref(),
                            trace_id,
                        )
                    });
                let credential_identity = credential
                    .as_deref()
                    .map(conduit_services::credential_fingerprint);
                let endpoint_base_url =
                    (!r.endpoint.base_url.is_empty()).then(|| r.endpoint.base_url.clone());
                let endpoint_path = (!r.endpoint.path.is_empty()).then(|| r.endpoint.path.clone());
                let endpoint_transport =
                    (!r.endpoint.transport.is_empty()).then(|| r.endpoint.transport.clone());
                PipelineCandidate {
                    id: r.channel_id.clone(),
                    base_url: endpoint_base_url.or_else(|| r.base_url.clone()),
                    credential,
                    credential_identity,
                    actual_model: r.models.first().map(|m| m.actual_model.clone()),
                    api_format: if r.endpoint.api_format.is_empty() {
                        r.api_format.clone()
                    } else {
                        r.endpoint.api_format.clone()
                    },
                    endpoint_path,
                    endpoint_transport,
                    channel_type: r.channel_type.clone(),
                    channel_config: build_channel_config(r),
                    // Per-channel retry overrides flow onto the candidate at
                    // selection time so the pipeline's same-channel retry gate is a
                    // zero-allocation slice check on the (cold) retry path — no
                    // `ChannelSettings` reconstruction per attempt. The default
                    // 429/5xx set is handled by the pipeline's `can_retry` default;
                    // these are the channel's *additional* opt-in codes/patterns
                    // (Go `Settings.RetryableStatusCodes` / `RetryableErrorPatterns`).
                    retryable_status_codes: r
                        .settings
                        .as_ref()
                        .map(|s| s.retryable_status_codes.clone())
                        .unwrap_or_default(),
                    retryable_error_patterns: r
                        .settings
                        .as_ref()
                        .map(|s| s.retryable_error_patterns.clone())
                        .unwrap_or_default(),
                    error_response_rewrite_rules: r
                        .settings
                        .as_ref()
                        .map(|s| s.error_response_rewrite_rules.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();

        let ordered_snapshot: Vec<Value> = healthy_ordered_indices
            .iter()
            .filter_map(|index| resolved.get(*index))
            .enumerate()
            .map(|(rank, candidate)| {
                json!({
                    "rank": rank + 1,
                    "channelID": candidate.channel_id,
                    "channelName": candidate.channel_name,
                    "priority": candidate.priority,
                    "orderingWeight": candidate.ordering_weight,
                    "actualModel": candidate.models.first().map(|model| model.actual_model.as_str()),
                    "apiFormat": candidate.api_format,
                })
            })
            .collect();
        ctx.metadata.insert(
            "route_requested_model".to_owned(),
            candidate_request.model.clone(),
        );
        if let Ok(value) = serde_json::to_string(&ordered_snapshot) {
            ctx.metadata
                .insert("route_ordered_candidates".to_owned(), value);
        }

        Ok(ordered_candidates)
    }

    /// RUST-P8-003 (phase 2) — the **live streaming** sibling of
    /// [`Self::process_command`] (Go `orchestrator.go:331-335`: `if result.Stream
    /// { return ...EventStream }`).
    ///
    /// Runs Select → LoadBalance (shared), then hands the request to
    /// [`Pipeline::stream_live`] to obtain the *incremental* upstream event
    /// receiver ([`Executor::execute_stream_live`]). It then wires the
    /// built-but-previously-unwired live components:
    ///
    /// * a [`PersistentStreamFinalizer`] carrying the attempt's execution id
    ///   (`{request_id}-attempt-{sequence}`), the [`RequestRecorder`], and a
    ///   [`TransformerStreamAggregator`] (usage-lifting, step 3);
    /// * an [`OutboundForwardingStream`] that forwards each upstream event to
    ///   the client while buffering into the finalizer, handles client-disconnect
    ///   → upstream cancel, and finalizes via `record_stream_final` /
    ///   `record_stream_request_chunks` at close **instead of** the non-stream
    ///   `record_success`.
    ///
    /// Returns a [`CommandStreamHandle`] carrying the client-facing
    /// `mpsc::Receiver<StreamEvent>` (a live stream — frames arrive as the
    /// provider emits them) and the `JoinHandle` for the persistence finalizer
    /// (awaitable by callers/tests; detached by the bridge in production).
    ///
    /// The buffered [`Self::process_command`] remains the path for non-stream
    /// requests, so nothing regresses — the bridge picks the branch by the
    /// user's stream flag.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_command_stream(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: std::sync::Arc<dyn conduit_transformers::InboundTransformer>,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> OrchestratorResult<CommandStreamHandle> {
        self.process_command_stream_impl(
            ctx,
            inbound,
            request_id,
            project_id,
            candidate_request,
            http_request,
            raw_inbound,
            trace_id,
            thread_id,
            None,
        )
        .await
    }

    /// Run the live-stream command flow using candidates already resolved by
    /// the caller, avoiding duplicate DB and quota lookups in the HTTP bridge.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_command_stream_with_resolved_candidates(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: std::sync::Arc<dyn conduit_transformers::InboundTransformer>,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        resolved_candidates: &[ChannelModelsCandidate],
    ) -> OrchestratorResult<CommandStreamHandle> {
        self.process_command_stream_impl(
            ctx,
            inbound,
            request_id,
            project_id,
            candidate_request,
            http_request,
            raw_inbound,
            trace_id,
            thread_id,
            Some(resolved_candidates),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_command_stream_impl(
        &self,
        ctx: &mut OrchestratorContext,
        inbound: std::sync::Arc<dyn conduit_transformers::InboundTransformer>,
        request_id: &str,
        project_id: &str,
        candidate_request: &CandidateRequest,
        http_request: HttpRequest,
        raw_inbound: &HttpRequest,
        trace_id: Option<&str>,
        thread_id: Option<&str>,
        resolved_candidates: Option<&[ChannelModelsCandidate]>,
    ) -> OrchestratorResult<CommandStreamHandle> {
        use crate::outbound_stream::{
            OutboundForwardingStream, PersistentStreamFinalizer, StreamChunkAggregator,
            TransformerStreamAggregator, UpstreamItem,
        };

        // ---- Client disconnect check (Go `ctx.Err()` at the top of Process) ----
        if self.cancel_token.is_canceled() {
            ctx.record_stage(OrchestratorStage::Pipeline);
            return Err(OrchestratorError::new(
                OrchestratorStage::Pipeline,
                ConduitError::internal("request canceled before pipeline"),
            ));
        }

        // ---- S01 Select + S02 LoadBalance ----
        let runtime_retry_policy = self.runtime_retry_policy().await;
        let load_balancer_retry_policy = runtime_retry_policy
            .map(|policy| policy.load_balancer)
            .unwrap_or(self.retry_policy);
        let load_balance_strategy =
            self.request_load_balance_strategy(&http_request, load_balancer_retry_policy);
        ctx.metadata.insert(
            LOAD_BALANCE_STRATEGY_METADATA.to_string(),
            load_balance_strategy.as_str().to_string(),
        );
        let ordered_candidates = self
            .resolve_ordered_candidates(
                ctx,
                request_id,
                candidate_request,
                load_balance_strategy,
                load_balancer_retry_policy,
                runtime_retry_policy.map(|policy| policy.cost_score_weight),
                trace_id,
                thread_id,
                resolved_candidates,
            )
            .await?;

        let admission = BillingAdmissionInput {
            request_key: request_id.to_string(),
            project_id: project_id.to_string(),
            api_key_id: http_request
                .metadata
                .get("api_key_id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    http_request
                        .metadata
                        .get("api_key_id")
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                }),
            public_model: candidate_request.model.clone(),
            estimated_input_tokens: estimate_candidate_input_tokens(candidate_request),
            max_output_tokens: candidate_request
                .max_output_tokens
                .map(u64::from)
                .unwrap_or(4096),
        };
        let api_key_limit = http_request
            .metadata
            .get("api_key_max_concurrent")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        self.recorder
            .admit_request(ctx, &admission, api_key_limit)
            .await
            .map_err(|error| OrchestratorError::new(OrchestratorStage::Quota, error))?;
        let mut admission_guard = RequestAdmissionGuard::new(
            Arc::clone(&self.recorder),
            ctx,
            "stream request future dropped before finalizer handoff",
        );

        // ---- S03 Pipeline (live): inbound → outbound → execute_stream_live ----
        let mut pipeline_ctx = PipelineContext::new();
        for (key, value) in &http_request.metadata {
            if let Some(s) = value.as_str() {
                pipeline_ctx.metadata.insert(key.clone(), s.to_string());
            } else if let Some(n) = value.as_i64() {
                pipeline_ctx.metadata.insert(key.clone(), n.to_string());
            }
        }
        pipeline_ctx.metadata.insert(
            LOAD_BALANCE_STRATEGY_METADATA.to_string(),
            load_balance_strategy.as_str().to_string(),
        );

        // S13 — per-request upstream cancel token, shared by the executor's
        // read loop and the forward loop's client-disconnect handler.
        let upstream_cancel = conduit_pipeline::CancelToken::new();
        let live = self
            .pipeline
            .stream_live_with_inbound(
                &mut pipeline_ctx,
                std::sync::Arc::clone(&inbound),
                http_request,
                raw_inbound,
                &ordered_candidates,
                upstream_cancel.clone(),
            )
            .await;
        let live = match live {
            Ok(live) => live,
            Err(error) => {
                // Admission already reserved customer funds. A stream that
                // fails before returning its live handle has no finalizer, so
                // release through the recorder here instead of waiting for
                // reservation expiry.
                let persisted_request_id = pipeline_ctx
                    .metadata
                    .get("__persist_request_id")
                    .map(String::as_str)
                    .unwrap_or(request_id);
                if let Err(record_error) = self
                    .recorder
                    .record_failure(ctx, persisted_request_id, project_id, &error)
                    .await
                {
                    return Err(OrchestratorError::new(
                        OrchestratorStage::Persist,
                        record_error,
                    ));
                }
                admission_guard.disarm();
                return Err(OrchestratorError::new(OrchestratorStage::Pipeline, error));
            }
        };
        let response_content_type = live.content_type.clone();

        // The live pipeline owns the attempt-scoped channel/model metadata.
        // Copy the fields needed by the detached usage/cost recorder before
        // moving the pipeline cleanup into its background finalizer task.
        for key in [
            "api_key_id",
            "trace_id",
            "client_ip",
            "data_storage_id",
            "actual_model",
            "request_model",
            "channel_id",
            "credential_identity",
            "format",
            "perf_outbound_start_ms",
        ] {
            if let Some(value) = pipeline_ctx.metadata.get(key) {
                ctx.metadata.insert(key.to_string(), value.clone());
            }
        }

        ctx.metadata
            .insert("pipeline_steps".to_string(), pipeline_ctx.order.join(","));

        // The live pipeline has now created the canonical database identities.
        // The bridge's pre-persist request id can be empty, so finalization
        // must use the ids published by the persistence middlewares.
        let persisted_request_id = pipeline_ctx
            .metadata
            .get("__persist_request_id")
            .cloned()
            .unwrap_or_else(|| request_id.to_string());
        let persisted_project_id = pipeline_ctx
            .metadata
            .get("__persist_project_id")
            .cloned()
            .unwrap_or_else(|| project_id.to_string());

        // ---- S04 Persist wiring: finalizer + forward loop (Go stream wrappers) ----
        ctx.record_stage(OrchestratorStage::Persist);

        // PersistRequestExecutionMiddleware allocates the real DB execution id
        // and stores it in the pipeline context. Reconstructing an in-memory
        // `{request}-attempt-{sequence}` id here can never address the
        // PostgreSQL execution row, leaving live SSE executions stuck in
        // `processing`.
        let execution_id = pipeline_ctx
            .metadata
            .get("__persist_execution_id")
            .cloned()
            .unwrap_or_else(|| format!("{request_id}-attempt-{}", live.sequence));
        let attempt = PipelineAttempt {
            sequence: live.sequence,
            channel_id: live.channel_id.clone(),
            model_index: live.model_index,
            mode: conduit_pipeline::pipeline::ExecutionMode::Stream,
            outcome: Ok(HttpResponse::default()),
        };
        let aggregator: Arc<dyn StreamChunkAggregator> =
            Arc::new(TransformerStreamAggregator::new(inbound));
        let finalizer = PersistentStreamFinalizer::new(
            aggregator,
            Arc::clone(&self.recorder),
            persisted_request_id,
            persisted_project_id,
            attempt,
        )
        .with_execution_id(execution_id);

        // Adapter (task option 1a): pipeline `Result<StreamEvent, ConduitError>`
        // receiver → orchestrator `UpstreamItem` sender the forward loop consumes.
        let (up_tx, up_rx) = tokio::sync::mpsc::channel::<UpstreamItem>(64);
        let mut upstream_rx = live.upstream_rx;
        tokio::spawn(async move {
            while let Some(item) = upstream_rx.recv().await {
                let (msg, stop) = match item {
                    Ok(event) => (UpstreamItem::Event(event), false),
                    Err(err) => (UpstreamItem::Error(err), true),
                };
                if up_tx.send(msg).await.is_err() {
                    break;
                }
                // A provider error is terminal — the stream yields nothing more.
                if stop {
                    break;
                }
            }
        });

        // Forward-while-aggregating loop → client-facing receiver.
        let (client_tx, client_rx) =
            tokio::sync::mpsc::channel::<Result<StreamEvent, ConduitError>>(64);
        let forwarding =
            OutboundForwardingStream::new(up_rx, client_tx, upstream_cancel, finalizer);
        let stream_cleanup = live.cleanup;
        // The finalizer needs an owned context (the request context is borrowed
        // by the caller); OrchestratorContext is Clone.
        let run_ctx = ctx.clone();
        let finalizer_handle = tokio::spawn(async move {
            let _stream_cleanup = stream_cleanup;
            let result = forwarding.run(&run_ctx).await;
            if result.is_ok() {
                admission_guard.disarm();
            }
            result
        });

        Ok(CommandStreamHandle {
            client_rx,
            content_type: response_content_type,
            finalizer: finalizer_handle,
        })
    }
}

/// RUST-P8-003 (phase 2) — the live-stream result of
/// [`CommandOrchestrator::process_command_stream`].
///
/// Carries the client-facing incremental event receiver plus the persistence
/// finalizer's `JoinHandle`. Production (the bridge) forwards `client_rx` to the
/// HTTP SSE writer and detaches `finalizer` (the persist runs independently);
/// tests await `finalizer` to assert the persisted rows.
pub struct CommandStreamHandle {
    /// Client-facing stream: each [`StreamEvent`] arrives as the provider emits
    /// it (forward-while-aggregating), terminated by the provider's own
    /// terminal event (e.g. `[DONE]`). Closes when the stream ends or the
    /// upstream is canceled.
    pub client_rx: tokio::sync::mpsc::Receiver<Result<StreamEvent, ConduitError>>,
    /// Provider response `Content-Type`, available before the first body chunk.
    /// Binary speech uses it for the Axum response header; SSE writers retain
    /// their fixed protocol content type.
    pub content_type: Option<String>,
    /// The persistence finalizer task (Go `OutboundPersistentStream.Close`):
    /// resolves to the [`StreamFinalPlan`] once the stream ends, having written
    /// the execution/request rows + chunks + usage.
    pub finalizer: tokio::task::JoinHandle<Result<StreamFinalPlan, ConduitError>>,
}

// ===========================================================================
// RUST-P9-006 S05 — stream.EnsureUsage (Go: `llm/pipeline/stream/usage.go`)
// ===========================================================================
//
// Go source (`conduit/llm/pipeline/stream/usage.go`):
//
//   func EnsureUsage() pipeline.Middleware {
//       return pipeline.OnLlmRequest("stream-usage", func(ctx context.Context, request *llm.Request) (*llm.Request, error) {
//           if request.Stream != nil && *request.Stream {
//               if request.StreamOptions == nil {
//                   request.StreamOptions = &llm.StreamOptions{}
//               }
//               request.StreamOptions.IncludeUsage = true
//           }
//           return request, nil
//       })
//   }
//
// In the Rust port the `StreamOptions` live on the chat payload as
// `ChatRequest.stream_options: Option<Value>` (a free-form JSON value, mirroring
// the Go `llm.StreamOptions` JSON shape — see `conduit-llm/src/model.rs:61`).
// `LlmRequest.stream` is a plain `bool` (no nil). So the middleware becomes a
// pure mutation: when streaming, force `stream_options.include_usage = true`,
// creating the object if it was missing. The decision is exposed via
// [`EnsureUsageOutcome`] so callers (and tests) can observe whether synthesis
// happened without re-inspecting the payload.
//
// `[Hadamard ?]`: the Go `llm.StreamOptions` struct is `{ IncludeUsage bool }`.
// Our `stream_options` is `Option<Value>`; the canonical JSON token is
// `"include_usage"` (snake_case — confirmed by the round-trip test in
// `conduit-llm/src/model.rs:390`). Providers that send extra fields on the
// object are preserved (we mutate in place when the value is an object).

/// Outcome of [`ensure_usage`] describing what the middleware did.
///
/// - [`EnsureUsageOutcome::NotStreaming`] — `request.stream == false`; the
///   middleware is a no-op (mirrors Go's `if request.Stream != nil && *request.Stream`).
/// - [`EnsureUsageOutcome::ForcedIncludeUsage`] — the request is streaming and
///   the middleware set `stream_options.include_usage = true` (creating the
///   object when absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureUsageOutcome {
    /// Streaming request; `include_usage` was forced on (the object may have
    /// been freshly created). `created` is true when `stream_options` was
    /// `None` before the call.
    ForcedIncludeUsage { created: bool },
    /// Non-streaming request — left untouched.
    NotStreaming,
}

/// JSON key for the OpenAI stream-usage flag (Go `llm.StreamOptions.IncludeUsage`
/// serializes to `include_usage`).
pub const STREAM_OPTIONS_INCLUDE_USAGE_KEY: &str = "include_usage";

/// S05 — Force `stream_options.include_usage = true` on a streaming
/// [`LlmRequest`] (Go `stream.EnsureUsage().OnInboundLlmRequest`).
///
/// Behavior faithfully mirrors `conduit/llm/pipeline/stream/usage.go`:
/// 1. When `request.stream` is false, do nothing and return
///    [`EnsureUsageOutcome::NotStreaming`].
/// 2. Otherwise:
///    - if `stream_options` is `None`, create a fresh `{"include_usage": true}`
///      object and store it on the chat payload;
///    - if `stream_options` is `Some(Value::Object)`, set the
///      `"include_usage"` key to `true` (preserving any other keys);
///    - if `stream_options` is `Some(non-object)`, replace it with
///      `{"include_usage": true}` (defensive — the Go struct round-trip would
///      never produce a non-object here, but we never panic).
///
/// Returns [`EnsureUsageOutcome::ForcedIncludeUsage`] with `created` reflecting
/// whether the object was freshly allocated.
///
/// `[Hadamard ?]`: if `request.payload` is not the chat variant, Go's middleware
/// still mutates `llm.Request.StreamOptions` (a top-level field in Go). The Rust
/// port stores `stream_options` on `ChatRequest`; when the payload is not Chat
/// we coerce to a default chat payload (mirroring the Go "always writable"
/// semantics used by `pre_execution::apply_auto_reasoning_effort`).
pub fn ensure_usage(request: &mut LlmRequest) -> EnsureUsageOutcome {
    if !request.stream {
        return EnsureUsageOutcome::NotStreaming;
    }

    let created = force_include_usage_on_chat_payload(chat_payload_or_default_mut_inline(request));
    EnsureUsageOutcome::ForcedIncludeUsage { created }
}

/// Inline equivalent of `pre_execution::chat_payload_or_default_mut` (which is
/// private). Mirrors Go's free-form mutation of `llm.Request.StreamOptions`: if
/// the payload is not the chat variant, coerce to a default chat payload first
/// so the middleware remains "always writable".
fn chat_payload_or_default_mut_inline(request: &mut LlmRequest) -> &mut conduit_llm::ChatRequest {
    use conduit_llm::{ChatRequest, LlmRequestPayload};
    if !matches!(request.payload, LlmRequestPayload::Chat(_)) {
        request.payload = LlmRequestPayload::Chat(ChatRequest::default());
    }
    match &mut request.payload {
        LlmRequestPayload::Chat(chat) => chat,
        _ => unreachable!("payload was just forced to Chat"),
    }
}

/// Mutate `chat.stream_options` so `include_usage == true`. Returns whether the
/// object was freshly created (was `None` or a non-object value).
fn force_include_usage_on_chat_payload(chat: &mut conduit_llm::ChatRequest) -> bool {
    match &mut chat.stream_options {
        Some(Value::Object(map)) => {
            map.insert(
                STREAM_OPTIONS_INCLUDE_USAGE_KEY.to_string(),
                Value::Bool(true),
            );
            false
        }
        Some(_) => {
            // Defensive: a non-object stream_options cannot carry the flag, so
            // overwrite it with the canonical object. Never panics.
            chat.stream_options = Some(stream_options_with_include_usage());
            true
        }
        None => {
            chat.stream_options = Some(stream_options_with_include_usage());
            true
        }
    }
}

/// Build the canonical `{"include_usage": true}` JSON object (Go
/// `&llm.StreamOptions{IncludeUsage: true}` marshals to exactly this).
fn stream_options_with_include_usage() -> Value {
    let mut map = serde_json::Map::new();
    map.insert(
        STREAM_OPTIONS_INCLUDE_USAGE_KEY.to_string(),
        Value::Bool(true),
    );
    Value::Object(map)
}

// ===========================================================================
// RUST-P9-006 S06 — enforceQuota (Go: `internal/server/orchestrator/quota.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/quota.go`):
//
//   func enforceQuota(inbound *PersistentInboundTransformer, quotaService *biz.QuotaService) pipeline.Middleware {
//       return pipeline.OnLlmRequest("enforce-quota", func(ctx context.Context, llmRequest *llm.Request) (*llm.Request, error) {
//           if quotaService == nil {
//               return llmRequest, nil
//           }
//           apiKey := inbound.state.APIKey
//           if apiKey == nil {
//               return llmRequest, nil
//           }
//           profile := apiKey.GetActiveProfile()
//           if profile == nil || profile.Quota == nil {
//               return llmRequest, nil
//           }
//           result, err := quotaService.CheckAPIKeyQuota(ctx, apiKey.ID, profile.Quota)
//           if err != nil {
//               return nil, err
//           }
//           if result.Allowed {
//               return llmRequest, nil
//           }
//           requestID, _ := contexts.GetRequestID(ctx)
//           fields := []log.Field{ ... }
//           log.Info(ctx, "api key quota exceeded", fields...)
//           return nil, &llm.ResponseError{
//               StatusCode: http.StatusForbidden,
//               Detail: llm.ErrorDetail{
//                   Code:      "quota_exceeded",
//                   Message:   result.Message,
//                   Type:      "quota_exceeded_error",
//                   RequestID: requestID,
//               },
//           }
//       })
//   }
//
// The Go middleware has two halves:
//   (a) the "should we even check?" gating (quotaService/apiKey/profile/quota
//       nil-shortcuts) — this is wired to the inbound state, which is not yet
//       ported, so it stays at the orchestrator-wiring layer;
//   (b) the **pure decision** over a `QuotaCheckResult` returned by
//       `quotaService.CheckAPIKeyQuota` — admit, or reject with a quota-exceeded
//       error carrying `result.Message`. THIS is the testable piece we port here.
//
// The pure `enforce_quota(quota_result)` below takes a typed
// [`QuotaCheckResultView`] (mirroring Go `biz.QuotaCheckResult{Allowed, Message,
// Window}`) and maps a denied result to `ConduitError::quota_exhausted(...)`. The
// mapping preserves the Go error shape:
//   - `code = "quota_exceeded"`
//   - `type = "quota_exceeded_error"`
//   - http status 403 (Go `http.StatusForbidden`)
//
// `[Hadamard ?]`: the workspace `ConduitError::quota_exhausted` helper sets
// `code = "quota_exhausted"` and `http_status = 429` (see `conduit-core::error`
// lines 213-218 + 66). The Go orchestrator emits a different shape
// (`quota_exceeded`, 403). To preserve Go parity we override `code` to
// `"quota_exceeded"` and `http_status` to 403 via the `with_code` /
// `with_http_status` builders, while still routing through the
// `QuotaExhausted` kind so the error-type taxonomy stays consistent. The
// `safe_message` carries `result.Message` (the human-readable denial reason
// from the quota service).

/// Typed view of a `biz.QuotaCheckResult` for the pure enforce-quota decision.
/// Mirrors Go `biz.QuotaCheckResult{Allowed bool; Message string; Window QuotaWindow}`.
///
/// The window fields are kept as `Option<chrono::DateTime<Utc>>` to match Go's
/// `*time.Time` (parity rule: `*T` -> `Option<T>`). They are surfaced for
/// diagnostics/logging only — the admit/deny decision reads only `allowed`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaCheckResultView {
    /// Go `QuotaCheckResult.Allowed`.
    pub allowed: bool,
    /// Go `QuotaCheckResult.Message` — the human-readable denial reason.
    pub message: String,
    /// Go `QuotaCheckResult.Window` — the quota window the denial falls in.
    pub window: QuotaWindowView,
}

/// Typed view of `biz.QuotaWindow`. Mirrors Go
/// `QuotaWindow{Start *time.Time; End *time.Time; EndInclusive bool}`.
///
/// The window timestamps are kept as ISO-8601 strings (`Option<String>`)
/// rather than typed `DateTime`s to avoid pulling `chrono` into the
/// orchestrator crate. The Go fields exist only for diagnostics/logging; the
/// pure enforce-quota decision reads only `allowed` on
/// [`QuotaCheckResultView`]. The orchestrator wiring (which already depends on
/// `chrono` via `conduit-core`/`conduit-llm`) formats `time.Time` into these
/// strings before calling [`enforce_quota`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaWindowView {
    /// Go `QuotaWindow.Start` formatted as RFC3339 (Go's default `time.Time`
    /// marshal format).
    pub start: Option<String>,
    /// Go `QuotaWindow.End` formatted as RFC3339.
    pub end: Option<String>,
    /// Go `QuotaWindow.EndInclusive`.
    pub end_inclusive: bool,
}

impl QuotaCheckResultView {
    /// Build an "allowed" result with an empty message (mirrors Go's
    /// `QuotaCheckResult{Allowed: true}` fast-path).
    pub fn allowed() -> Self {
        Self {
            allowed: true,
            ..Default::default()
        }
    }

    /// Build a "denied" result carrying the Go `result.Message`.
    pub fn denied(message: impl Into<String>) -> Self {
        Self {
            allowed: false,
            message: message.into(),
            ..Default::default()
        }
    }
}

/// HTTP status code the Go orchestrator returns on quota denial
/// (`http.StatusForbidden`).
pub const QUOTA_EXCEEDED_HTTP_STATUS: u16 = 403;

/// Error `code` string emitted by the Go orchestrator on quota denial
/// (`llm.ResponseError.Detail.Code = "quota_exceeded"`).
pub const QUOTA_EXCEEDED_CODE: &str = "quota_exceeded";

/// Error `type` string emitted by the Go orchestrator on quota denial
/// (`llm.ResponseError.Detail.Type = "quota_exceeded_error"`). Surfaced as a
/// constant so tests can assert the Go parity shape without hardcoding the
/// literal.
pub const QUOTA_EXCEEDED_ERROR_TYPE: &str = "quota_exceeded_error";

/// S06 — Pure enforce-quota decision over a [`QuotaCheckResultView`].
///
/// Mirrors the post-`CheckAPIKeyQuota` branch of Go `enforceQuota`:
/// - `Ok(())` when `result.allowed`;
/// - `Err(ConduitError)` of kind `QuotaExhausted` when denied, carrying
///   `result.message` as the safe/public message, with `code` overridden to
///   [`QUOTA_EXCEEDED_CODE`] (`"quota_exceeded"`) and `http_status` to
///   [`QUOTA_EXCEEDED_HTTP_STATUS`] (403) to match the Go `llm.ResponseError`
///   shape.
///
/// The nil-gating (`quotaService == nil`, `apiKey == nil`, `profile.Quota == nil`)
/// is the orchestrator wiring's responsibility — when the wiring determines no
/// check is needed it should call this with [`QuotaCheckResultView::allowed`]
/// (or skip the call entirely). This keeps the pure decision free of the
/// not-yet-ported inbound state.
pub fn enforce_quota(result: &QuotaCheckResultView) -> Result<(), ConduitError> {
    if result.allowed {
        return Ok(());
    }

    let message = if result.message.is_empty() {
        // Go's biz layer always populates Message on denial; defensively fall
        // back to the kind's default safe message if it is somehow empty.
        "quota exceeded".to_string()
    } else {
        result.message.clone()
    };

    let err = ConduitError::quota_exhausted(message)
        .with_code(QUOTA_EXCEEDED_CODE)
        .with_http_status(QUOTA_EXCEEDED_HTTP_STATUS);

    Err(err)
}

// ===========================================================================
// RUST-P9-006 S11 — injectPrompts (Go: `internal/server/orchestrator/prompt.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/prompt.go`):
//
//   func injectPrompts(inbound *PersistentInboundTransformer) pipeline.Middleware {
//       matcher := biz.NewPromptMatcher()
//       return pipeline.OnLlmRequest("inject-prompts", func(ctx context.Context, llmRequest *llm.Request) (*llm.Request, error) {
//           projectID, ok := contexts.GetProjectID(ctx)
//           if !ok { ...; return llmRequest, nil }
//           enabledPrompts, err := inbound.state.PromptProvider.GetEnabledPrompts(ctx, projectID)
//           if err != nil { ...; return llmRequest, nil }
//           if len(enabledPrompts) == 0 { return llmRequest, nil }
//           var apiKeyID int
//           if apiKey, ok := contexts.GetAPIKey(ctx); ok { apiKeyID = apiKey.ID }
//           matchingPrompts := matcher.FilterMatchingPrompts(enabledPrompts, llmRequest.Model, apiKeyID)
//           if len(matchingPrompts) == 0 { ...; return llmRequest, nil }
//           llmRequest = matcher.ApplyPrompts(llmRequest, matchingPrompts)
//           return llmRequest, nil
//       })
//   }
//
// The Go middleware is a thin adapter around two pieces of pure logic that have
// already been ported in `conduit-services`:
//   1. `biz.PromptMatcher.FilterMatchingPrompts(prompts, model, apiKeyID)` —
//      exposed as [`conduit_services::PromptMatcher::filter_matching_prompts`].
//   2. `biz.PromptMatcher.ApplyPrompts(request, matchingPrompts)` — exposed as
//      [`conduit_services::inject_prompts`].
//
// This file ports only the **bridge** between the orchestrator's
// [`LlmRequest`] shape and those pure helpers. The context/state plumbing
// (`contexts.GetProjectID`, `inbound.state.PromptProvider.GetEnabledPrompts`,
// `contexts.GetAPIKey`) lives in the wiring layer (`PersistentInboundTransformer`,
// not yet ported) — the pure bridge here takes the already-resolved `&[Prompt]`
// slice the wiring produces, mirroring the role the S05/S06/S07..S19 siblings
// play.
//
// Parity details:
//   * Go `llm.Request.Messages` <-> Rust `LlmRequest.payload =
//     LlmRequestPayload::Chat(ChatRequest { messages, .. })`. The Go middleware
//     always operates on `llmRequest.Messages`; if the Rust payload is not the
//     Chat variant, we coerce it to a default chat payload (mirrors Go's
//     "always writable" semantics used by [`ensure_usage`] and
//     `pre_execution::apply_auto_reasoning_effort`).
//   * Go's `ApplyPrompts` returns a new `*llm.Request` (it reassigns
//     `llmRequest`). The Rust port mutates in place because the orchestrator
//     owns the [`LlmRequest`] uniquely.
//   * Go `apiKey.ID` is an `int`. The Rust [`PromptMatcher`] takes `i64`
//     (parity rule: Go `int` ids -> Rust `i64`). Callers pass `0` when no
//     API key is in context, matching Go's zero-value.
//   * The Go matcher runs *before* injection; we mirror that by calling
//     [`PromptMatcher::filter_matching_prompts`] inside the bridge so callers
//     pass the full enabled set (exactly the slice the wiring would have).

/// Outcome of [`apply_inject_prompts`] describing what the bridge did.
///
/// - [`InjectPromptsOutcome::Skipped`] — no enabled prompts were supplied (mirrors
///   Go's `len(enabledPrompts) == 0` fast path) or none matched the model/api-key
///   (mirrors Go's `len(matchingPrompts) == 0` fast path). The request is left
///   untouched.
/// - [`InjectPromptsOutcome::Injected`] — at least one prompt was injected. The
///   chat payload's `messages` was replaced with the new
///   `prepend ++ original ++ append` ordering. `matched` carries the per-prompt
///   reasons (S12 surface) in the order prompts were applied.
#[derive(Debug, Clone, PartialEq)]
pub enum InjectPromptsOutcome {
    /// Prompt injection was applied. Carries the per-prompt match reasons.
    Injected {
        /// Per-prompt reasons, in apply order (prepend bucket first, then
        /// append). Mirrors the [`conduit_services::PromptInjectionReason`] list
        /// produced by [`conduit_services::inject_prompts`].
        matched: Vec<conduit_services::PromptInjectionReason>,
    },
    /// No prompts were supplied, or none matched the model/api-key. The request
    /// was left unchanged. `enabled_count` is the size of the input slice
    /// (0 in the Go `len(enabledPrompts) == 0` branch).
    Skipped {
        /// Size of the enabled-prompts slice the bridge was asked to apply.
        enabled_count: usize,
    },
}

/// S11 — Bridge the orchestrator's [`LlmRequest`] into the pure
/// [`conduit_services::inject_prompts`] helper (Go
/// `injectPrompts` middleware + `biz.PromptMatcher.ApplyPrompts`).
///
/// Mirrors the Go middleware's two steps:
/// 1. Filter `enabled_prompts` against `model` / `api_key_id` using
///    [`conduit_services::PromptMatcher::filter_matching_prompts`] (Go
///    `matcher.FilterMatchingPrompts`).
/// 2. When at least one prompt survives, apply them via
///    [`conduit_services::inject_prompts`] (Go `matcher.ApplyPrompts`) and
///    overwrite the chat payload's `messages` with the result.
///
/// The function is pure with respect to the request's *other* fields — only
/// `payload` (Chat messages) is mutated. When the payload is not the Chat
/// variant, it is coerced to a default chat payload first (Go's "always
/// writable" semantics; the original payload is lost, mirroring how
/// [`ensure_usage`] / `pre_execution::apply_auto_reasoning_effort` handle the
/// same case).
///
/// `[Democritus ?]`: the Go middleware resolves the API-key id from the context
/// (`contexts.GetAPIKey(ctx).ID`); here it is an explicit parameter the wiring
/// layer supplies. `0` means "no api key in context" (Go zero-value), which the
/// matcher treats as "does not satisfy any `api_key`-typed activation condition"
/// — matching Go behavior.
pub fn apply_inject_prompts(
    request: &mut LlmRequest,
    enabled_prompts: &[conduit_services::Prompt],
    model: &str,
    api_key_id: i64,
) -> InjectPromptsOutcome {
    // Go: `if len(enabledPrompts) == 0 { return llmRequest, nil }`.
    if enabled_prompts.is_empty() {
        return InjectPromptsOutcome::Skipped { enabled_count: 0 };
    }

    // Go: `matcher.FilterMatchingPrompts(enabledPrompts, llmRequest.Model, apiKeyID)`.
    let matcher = conduit_services::PromptMatcher::new();
    let matched_refs = matcher.filter_matching_prompts(enabled_prompts, model, api_key_id);

    // Go: `if len(matchingPrompts) == 0 { ...; return llmRequest, nil }`.
    if matched_refs.is_empty() {
        return InjectPromptsOutcome::Skipped {
            enabled_count: enabled_prompts.len(),
        };
    }

    // The matcher returns `Vec<&Prompt>`; `inject_prompts` takes `&[Prompt]`,
    // so clone the matched slice (the Go side also sorts its `matchingPrompts`
    // in place — cloning keeps our port pure, matching the helper's contract).
    let matched_owned: Vec<conduit_services::Prompt> =
        matched_refs.iter().map(|p| (*p).clone()).collect();

    let chat = chat_payload_or_default_mut_inline(request);
    let original_messages = std::mem::take(&mut chat.messages);

    // Go: `matcher.ApplyPrompts(llmRequest, matchingPrompts)`.
    let injection = conduit_services::inject_prompts(&original_messages, &matched_owned);
    chat.messages = injection.messages;

    InjectPromptsOutcome::Injected {
        matched: injection.matched,
    }
}

// ===========================================================================
// RUST-P9-006 S12 — protectPrompts (Go:
//   `internal/server/orchestrator/prompt_protection.go` +
//   `internal/server/biz/prompt_protection_request.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/prompt_protection.go`):
//
//   const promptProtectionRejectedMessage = "request blocked by prompt protection policy"
//
//   func protectPrompts(inbound *PersistentInboundTransformer) pipeline.Middleware {
//       return pipeline.OnLlmRequest("protect-prompts", func(ctx context.Context, llmRequest *llm.Request) (*llm.Request, error) {
//           if inbound.state.PromptProtecter == nil { return llmRequest, nil }
//           protected, err := inbound.state.PromptProtecter.Protect(ctx, llmRequest)
//           if err != nil {
//               if errors.Is(err, biz.ErrPromptProtectionRejected) {
//                   return nil, fmt.Errorf("%w: %s", transformer.ErrInvalidRequest, promptProtectionRejectedMessage)
//               }
//               log.Warn(ctx, "failed to protect prompts", log.Cause(err))
//               return llmRequest, nil
//           }
//           if protected == nil { return llmRequest, nil }
//           return protected, nil
//       })
//   }
//
// And (`internal/server/biz/prompt_protection_request.go`):
//
//   func ApplyPromptProtectionRules(req *llm.Request, rules []*ent.PromptProtectionRule) PromptProtectionResult {
//       if req == nil || len(req.Messages) == 0 || len(rules) == 0 {
//           return PromptProtectionResult{Request: req}
//       }
//       ... // per-rule / per-message loop; on first `reject` rule hit returns
//           // PromptProtectionResult{MatchedRules: []*ent.PromptProtectionRule{rule}, Rejected: true}
//           // without mutating further; on `mask` it rewrites the message text.
//   }
//
//   func (svc *PromptProtectionRuleService) Protect(ctx context.Context, req *llm.Request) (*llm.Request, error) {
//       rules, err := svc.ListEnabledRules(ctx)
//       if err != nil { ...; return nil, err }
//       if len(rules) == 0 { return req, nil }
//       result := ApplyPromptProtectionRules(req, rules)
//       if len(result.MatchedRules) == 0 { return req, nil }
//       if result.Rejected { ...; return result.Request, ErrPromptProtectionRejected }
//       return result.Request, nil
//   }
//
// The Go `Protect` method has two halves:
//   (a) the I/O half (`ListEnabledRules` + the policy short-circuits) — owned by
//       the wiring layer;
//   (b) the **pure decision** over a `&[PromptProtectionRule]` slice + the
//       `ApplyPromptProtectionRules` body — already ported in
//       `conduit-services::preview_protection` (which mirrors
//       `ApplyPromptProtectionRules` *and* additionally records per-rule / per-
//       message hit reasons for the S12 frontend surface).
//
// This file bridges the orchestrator's [`LlmRequest`] shape into
// [`conduit_services::preview_protection`] and maps the resulting
/// [`conduit_services::ProtectionPreview`] back onto the request, surfacing the
/// Go-level admit / reject decision via [`ProtectOutcome`].
//
// Parity details:
//   * Go's `promptProtectionRejectedMessage` is reproduced verbatim as
//     [`PROMPT_PROTECTION_REJECTED_MESSAGE`].
//   * On reject, the Go middleware wraps the error as
//     `fmt.Errorf("%w: %s", transformer.ErrInvalidRequest,
//     promptProtectionRejectedMessage)`. The Rust port routes through
//     [`ConduitError::invalid_request`] carrying that exact message, which yields
//     `error_type() = "invalid_request"` (matching
//     `transformer.ErrInvalidRequest`'s Rust counterpart) and the Go message as
//     both the diagnostic and safe/public message.
//   * On mask, Go mutates `req.Messages` in place and returns the same request;
//     the Rust port writes the masked `messages` back into the chat payload
//     (mutating in place, same observable effect).
//   * On no-match (no rule fired), Go returns the original `req`; the Rust port
//     leaves the request untouched and returns [`ProtectOutcome::Allow`] with
//     an empty `masked_by` list.
//   * `[Democritus ?]`: Go's `PromptProtecter.Protect` swallows non-rejected
//     errors with a `log.Warn` and admits the request. The pure bridge here
//     cannot fail (the underlying [`conduit_services::preview_protection`] is
//     infallible — invalid patterns are silently treated as non-matching,
//     mirroring Go's `compileErr` branch). The wiring layer is responsible for
//     surfacing I/O failures from `ListEnabledRules`.

/// Go literal: `promptProtectionRejectedMessage`
/// (`internal/server/orchestrator/prompt_protection.go`).
pub const PROMPT_PROTECTION_REJECTED_MESSAGE: &str = "request blocked by prompt protection policy";

/// Outcome of [`apply_protect_prompts`] describing the Go `Protect` decision.
///
/// Mirrors the three terminal branches of the Go `protectPrompts` middleware +
/// `Protect` method:
/// - [`ProtectOutcome::Allow`] — no rule fired (Go returns the original `req`),
///   or one or more `mask` rules fired and the request's messages were rewritten
///   in place (Go returns the rewritten `req`). `masked_by` lists the rule ids
///   that produced a rewrite, in the order they were evaluated (empty when no
///   rule fired).
/// - [`ProtectOutcome::Block`] — a `reject` rule fired (Go returns
///   `ErrPromptProtectionRejected`). Carries the id of the first rejecting rule
///   (Go logs `result.MatchedRules[0].Name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectOutcome {
    /// The request was admitted. When `masked_by` is non-empty, the chat
    /// payload's `messages` were rewritten in place by the matching `mask`
    /// rules.
    Allow {
        /// Ids of the `mask` rules that fired, in evaluation order. Empty when
        /// no rule fired (Go's `len(result.MatchedRules) == 0` branch).
        masked_by: Vec<String>,
    },
    /// The request was rejected by a `reject` rule. The chat payload is left
    /// untouched (Go short-circuits before applying any further mutations).
    Block {
        /// Id of the first `reject` rule that fired (Go's
        /// `result.MatchedRules[0]`).
        rejecting_rule_id: String,
        /// Display name of the rejecting rule (Go `result.MatchedRules[0].Name`),
        /// for diagnostics.
        rejecting_rule_name: String,
    },
}

impl ProtectOutcome {
    /// `true` when this outcome is [`ProtectOutcome::Allow`].
    pub const fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// `true` when this outcome is [`ProtectOutcome::Block`].
    pub const fn is_block(&self) -> bool {
        matches!(self, Self::Block { .. })
    }

    /// Build the [`ConduitError`] the Go middleware emits on a block (Go:
    /// `fmt.Errorf("%w: %s", transformer.ErrInvalidRequest,
    /// promptProtectionRejectedMessage)`). Routes through
    /// [`ConduitError::invalid_request`] so `error_type()` resolves to
    /// `"invalid_request"` (Rust counterpart of `transformer.ErrInvalidRequest`)
    /// and the message is [`PROMPT_PROTECTION_REJECTED_MESSAGE`].
    pub fn to_rejection_error(&self) -> ConduitError {
        ConduitError::invalid_request(PROMPT_PROTECTION_REJECTED_MESSAGE)
    }
}

/// S12 — Bridge the orchestrator's [`LlmRequest`] into the pure
/// [`conduit_services::preview_protection`] helper (Go `protectPrompts`
/// middleware + `biz.ApplyPromptProtectionRules`).
///
/// Mirrors the Go decision tree:
/// 1. Go `if len(rules) == 0 { return req, nil }` — when `rules` is empty,
///    returns [`ProtectOutcome::Allow`] with an empty `masked_by` and leaves the
///    request untouched.
/// 2. Otherwise runs [`conduit_services::preview_protection`] over the chat
///    payload's `messages` (Go `ApplyPromptProtectionRules`). This reproduces
///    the per-rule / per-message evaluation, the role-scope gating, the `mask`
///    in-place rewrite, and the **first-reject short-circuit**.
/// 3. On `rejected == true` (Go `result.Rejected`), returns
///    [`ProtectOutcome::Block`] carrying the first rejecting rule's id/name.
///    The chat payload is left untouched — Go's `ApplyPromptProtectionRules`
///    short-circuits at the first `reject` hit *before* mutating further (the
///    working messages slice is discarded via `messages: None` on the
///    `ProtectionPreview`).
/// 4. On `rejected == false`, writes the (possibly masked) `messages` back onto
///    the chat payload and returns [`ProtectOutcome::Allow`] with the ids of
///    every rule that matched at least one message.
///
/// `rules` mirrors the `&[(&PromptRule, PromptProtectionSettings)]` shape that
/// [`conduit_services::preview_protection`] takes: the wiring layer pairs each
/// loaded rule with its `Settings` (Go's `rule.Settings`). When `settings` is
/// `PromptProtectionSettings::default()` the rule has action `""` (treated as a
/// no-op by the underlying `apply_rule_to_text`); callers should generally
/// build the settings from the rule's `action`/`replacement`/`scopes`.
///
/// `[Democritus ?]`: when the payload is not the Chat variant, the bridge
/// coerces it to a default chat payload (mirrors Go's "always writable"
/// semantics in [`ensure_usage`] / [`apply_inject_prompts`]). The Go
/// `ApplyPromptProtectionRules` does not check the payload kind — it operates
/// purely on `req.Messages`, which is a top-level field in Go; in Rust the
/// messages live on the Chat payload, so the coercion is the faithful analog.
pub fn apply_protect_prompts(
    request: &mut LlmRequest,
    rules: &[(
        &conduit_services::PromptRule,
        conduit_core::objects::prompt_protection::PromptProtectionSettings,
    )],
) -> ProtectOutcome {
    // Go (via Protect): `if len(rules) == 0 { return req, nil }`.
    if rules.is_empty() {
        return ProtectOutcome::Allow {
            masked_by: Vec::new(),
        };
    }

    let chat = chat_payload_or_default_mut_inline(request);
    let messages = std::mem::take(&mut chat.messages);

    // Go: `ApplyPromptProtectionRules(req, rules)`. `preview_protection` is the
    // pure Rust port of that function (plus the S12 per-rule/per-message hit
    // reasons).
    let preview = conduit_services::preview_protection(&messages, rules);

    if preview.rejected {
        // Go: `result.Rejected == true` => short-circuit. The rejecting rule is
        // `result.MatchedRules[0]`. We surface it via the Block outcome; the
        // chat payload stays at its pre-rule state (messages already `take`n —
        // restore them so the request is observably untouched, matching Go's
        // "do not mutate on reject" behavior).
        chat.messages = messages;

        // The reject rule is the last entry pushed before the early return in
        // `preview_protection`; its `matched` is `true`.
        let rejector = preview.rules.iter().rev().find(|r| {
            r.matched
                && r.action
                    == conduit_core::objects::prompt_protection::PROMPT_PROTECTION_ACTION_REJECT
        });
        let (rejecting_rule_id, rejecting_rule_name) = match rejector {
            Some(rule) => (rule.rule_id.clone(), rule.rule_name.clone()),
            // Defensive: `preview_protection` always pushes the rejecting rule
            // before returning `rejected: true`; if the invariant breaks, fall
            // back to a sentinel so the outcome still clearly signals Block.
            None => ("unknown".to_string(), "unknown".to_string()),
        };

        return ProtectOutcome::Block {
            rejecting_rule_id,
            rejecting_rule_name,
        };
    }

    // Go: not rejected — `result.Request` carries the masked messages (or the
    // original when no rule fired). Write them back.
    match preview.messages {
        Some(masked) => {
            chat.messages = masked;
        }
        None => {
            // `preview_protection` only returns `None` on reject; restore the
            // originals as a defensive fallback.
            chat.messages = messages;
        }
    }

    let masked_by: Vec<String> = preview
        .rules
        .into_iter()
        .filter(|r| r.matched)
        .map(|r| r.rule_id)
        .collect();

    ProtectOutcome::Allow { masked_by }
}

// ===========================================================================
// RUST-P9-006 S20 — withPerformanceRecording (Go:
//   `internal/server/orchestrator/performance.go` +
//   `internal/server/biz/channel_metrics.go::PerformanceRecord`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/performance.go`):
//
//   func withPerformanceRecording(outbound *PersistentOutboundTransformer) pipeline.Middleware {
//       return &performanceRecording{outbound: outbound}
//   }
//
//   func (m *performanceRecording) OnInboundLlmRequest(ctx, request) {
//       if m.outbound.state.Perf == nil { m.outbound.state.Perf = &biz.PerformanceRecord{} }
//       if request.Stream != nil { m.outbound.state.Perf.Stream = *request.Stream } else { m.outbound.state.Perf.Stream = false }
//       return request, nil
//   }
//
//   func (m *performanceRecording) OnOutboundRawRequest(ctx, request) {
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil { return request, nil }
//       var streamFlag bool
//       if m.outbound.state.Perf != nil { streamFlag = m.outbound.state.Perf.Stream }   // <- bug fix from commit 8afd95c3
//       perf := biz.PerformanceRecord{}
//       perf.StartTime = time.Now()
//       perf.ChannelID = channel.ID
//       perf.Success = false
//       perf.RequestCompleted = false
//       perf.Stream = streamFlag
//       if apiKey, ok := contexts.GetChannelAPIKey(ctx); ok { perf.APIKey = apiKey }
//       m.outbound.state.Perf = &perf
//       return request, nil
//   }
//
//   // OnOutboundLlmResponse / OnOutboundRawError: MarkSuccess / MarkFailed / MarkCanceled
//   // OnOutboundLlmStream: recordPerformanceStream — MarkFirstToken on first event,
//   //   MarkReasoningStart on first non-empty ReasoningContent delta,
//   //   MarkReasoningEnd on the transition out of reasoning into content/tool_calls.
//
// Go `PerformanceRecord` marker methods (`channel_metrics.go:550-594`):
//
//   func (m *PerformanceRecord) MarkSuccess()      { m.Success=true; m.RequestCompleted=true; m.EndTime=time.Now() }
//   func (m *PerformanceRecord) MarkFirstToken()   { if m.FirstTokenTime == nil { now := time.Now(); m.FirstTokenTime = &now } }
//   func (m *PerformanceRecord) MarkReasoningStart() { if m.ReasoningStartTime == nil { ... } }
//   func (m *PerformanceRecord) MarkReasoningEnd()   { if m.ReasoningEndTime == nil { ... } }
//   func (m *PerformanceRecord) MarkFailed(code int) { m.Success=false; m.ResponseStatusCode=code; m.RequestCompleted=true; m.EndTime=time.Now() }
//   func (m *PerformanceRecord) MarkCanceled()       { m.Success=false; m.Canceled=true; m.RequestCompleted=true; m.EndTime=time.Now() }
//
// The Go middleware is heavily I/O-shaped (it owns a `*PersistenceState`, calls
// `ChannelService.AsyncRecordPerformance`, and wraps the stream). As with the
// S05/S06/S11/S12 siblings, we extract the **pure decision** the middleware
// makes — namely *which performance markers should be set, and when* — into a
// data-only [`RecordingPlan`] the wiring layer consumes. The wiring then calls
// the typed `PerformanceRecord::{mark_*}` setters on its own state object.
//
// The pure decisions mirror the Go code:
//   (1) On inbound, the recording is always active for the lifetime of the
//       attempt; the only field set on the inbound hook is `Stream`
//       (`OnInboundLlmRequest`). That decision is exposed as
//       [`RecordingPlan::for_inbound`].
//   (2) On the first stream event, `MarkFirstToken` fires exactly once (Go:
//       `if !s.firstTokenSet { s.state.Perf.MarkFirstToken(); s.firstTokenSet =
//       true }`). This is **stream-only** — non-stream responses go straight to
//       `MarkSuccess` in `OnOutboundLlmResponse` and never mark first-token.
//   (3) While streaming, `MarkReasoningStart` fires once on the first delta
//       whose `ReasoningContent` is non-empty, and `MarkReasoningEnd` fires once
//       on the first subsequent delta that leaves reasoning (content /
//       multiple_content / tool_calls). This transition is purely a function of
//       the observed delta kind — captured by [`RecordingMarker`] / the
//       [`stream_marker_decision`] helper.
//   (4) On success / failure / cancel, exactly one of
//       `MarkSuccess` / `MarkFailed` / `MarkCanceled` fires. Captured by
//       [`TerminalMarker`].
//
// `[Hertz ?]`: the `recordPerformanceStream.Current` helper also records the
// success on the *stream's* usage event (when `event.Usage.GetCompletionTokens()
// > 0`). That success-on-first-nonzero-usage behavior is part of the stream
// marker transition, surfaced here via [`stream_marker_decision`] returning
// [`RecordingMarker::RecordSuccess`] when the stream event carries non-zero
// completion tokens. The wiring layer applies that immediately (it is the Go
// `MarkSuccess()` + `AsyncRecordPerformance` call on the very event).

/// One observable stream-event kind that drives the performance markers (Go
/// `recordPerformanceStream.Current` looks at `event.Choices[0].Delta` and
/// `event.Usage` to decide which marker to fire).
///
/// Variants mirror the discriminating fields of Go's
/// `llm.Response`/`llm.Choice.Delta` that `recordPerformanceStream` reads:
///
/// * [`StreamEventKind::Reasoning`] — `delta.ReasoningContent != nil &&
///   *delta.ReasoningContent != ""` (Go triggers `MarkReasoningStart`).
/// * [`StreamEventKind::Content`] — `delta.Content.Content != nil &&
///   *delta.Content.Content != ""` **or** `len(delta.Content.MultipleContent) >
///   0` **or** `len(delta.ToolCalls) > 0` (Go triggers `MarkReasoningEnd` when
///   previously in reasoning).
/// * [`StreamEventKind::Usage`] — `event.Usage.GetCompletionTokens() != nil &&
///   *tokenCount > 0` (Go triggers `MarkSuccess` on the stream).
/// * [`StreamEventKind::Other`] — anything else (no marker fires; mirrors Go's
///   implicit fall-through where the event is returned unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEventKind {
    /// Non-empty reasoning content arrived (Go `MarkReasoningStart`).
    Reasoning,
    /// Non-empty content / multiple_content / tool_calls arrived (Go
    /// `MarkReasoningEnd` transition).
    Content,
    /// A usage chunk with non-zero completion tokens arrived (Go stream-side
    /// `MarkSuccess`).
    Usage,
    /// Anything else — no marker fires.
    Other,
}

/// A performance marker the wiring layer should apply to its `PerformanceRecord`
/// (the Rust analog of one of Go's `MarkFirstToken` / `MarkReasoningStart` /
/// `MarkReasoningEnd` / `MarkSuccess` calls).
///
/// Each variant is the *intent* ("set this field if unset"); the wiring layer
/// applies it idempotently (Go's setters are themselves idempotent — they only
/// set when `nil`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMarker {
    /// Go `MarkFirstToken()` — set `FirstTokenTime = now` if unset.
    FirstToken,
    /// Go `MarkReasoningStart()` — set `ReasoningStartTime = now` if unset.
    ReasoningStart,
    /// Go `MarkReasoningEnd()` — set `ReasoningEndTime = now` if unset.
    ReasoningEnd,
    /// Go stream-side `MarkSuccess()` — the stream emitted a usage chunk with
    /// non-zero completion tokens; record success and ship the perf record.
    RecordSuccess,
}

/// The terminal marker applied exactly once at the end of the attempt (Go
/// `OnOutboundLlmResponse` → `MarkSuccess`, `OnOutboundRawError` →
/// `MarkFailed`/`MarkCanceled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalMarker {
    /// Go `MarkSuccess()` (success path, non-stream).
    Success,
    /// Go `MarkFailed(errorCode)` (failure path, non-canceled error). Carries
    /// the extracted HTTP status code (Go `ExtractErrorCode`; defaults to 500).
    Failed { error_code: i32 },
    /// Go `MarkCanceled()` (`errors.Is(err, context.Canceled)`).
    Canceled,
}

/// The pure recording plan for one attempt — describes which stream markers may
/// fire (and from which events) and which terminal marker the wiring must apply.
///
/// Mirrors the Go `recordPerformanceStream` lifecycle hooks driven off
/// `m.outbound.state.Perf.Stream`:
/// * **Non-stream** attempts only ever see the terminal marker (`Success` /
///   `Failed` / `Canceled`). `first_token` / `reasoning` markers never fire.
/// * **Stream** attempts additionally fire `FirstToken` on the first event and
///   the reasoning markers on the matching transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingPlan {
    /// Whether this attempt is being recorded as a stream (Go
    /// `state.Perf.Stream`). When `false`, no per-event markers fire and only
    /// the [`TerminalMarker`] is applied.
    pub stream: bool,
}

impl RecordingPlan {
    /// Build the plan for an inbound request (Go `OnInboundLlmRequest` — the
    /// only piece of state set on inbound is `state.Perf.Stream`). `stream` is
    /// the request's stream flag (Go `*request.Stream`, with nil → false).
    pub fn for_inbound(stream: bool) -> Self {
        Self { stream }
    }

    /// Decide which stream marker (if any) the wiring layer should fire for a
    /// single observed stream event, given the marker's current "already-fired"
    /// state.
    ///
    /// Mirrors `recordPerformanceStream.Current` exactly:
    /// - The very first event (any kind) on a stream triggers `FirstToken`
    ///   (Go: `if !s.firstTokenSet { s.state.Perf.MarkFirstToken();
    ///   s.firstTokenSet = true }`).
    /// - `Reasoning` events trigger `ReasoningStart` (idempotent — only when
    ///   `reasoning_start_fired` is false).
    /// - The first `Content` event *after* reasoning has started triggers
    ///   `ReasoningEnd` (Go: `if s.reasoningStartSet && !s.reasoningEndSet {
    ///   MarkReasoningEnd() }`).
    /// - `Usage` events trigger `RecordSuccess` (Go stream-side success).
    ///
    /// `fired_state` carries the wiring's current "already fired" booleans so
    /// the decision is pure with respect to its inputs. The returned marker (if
    /// any) is what the wiring should apply *and* the corresponding `fired`
    /// flag flips to `true` afterward (mirrors Go's per-stream state).
    pub fn stream_marker(
        &self,
        event_kind: StreamEventKind,
        fired_state: MarkerFiredState,
    ) -> (Option<RecordingMarker>, MarkerFiredState) {
        let mut next = fired_state;

        if !self.stream {
            // Non-stream: no per-event markers. (Go only wraps the stream with
            // `recordPerformanceStream` when the response is streamed; the raw
            // HTTP path goes straight to OnOutboundLlmResponse.)
            return (None, next);
        }

        // 1) FirstToken fires on the very first event of any kind.
        let mut marker: Option<RecordingMarker> = None;
        if !next.first_token {
            marker = Some(RecordingMarker::FirstToken);
            next.first_token = true;
        }

        // 2) Reasoning / Content / Usage transitions.
        match event_kind {
            StreamEventKind::Reasoning => {
                if !next.reasoning_start {
                    // If we already had a pending FirstToken marker this event,
                    // the reasoning-start fires on the *next* event (Go runs
                    // them sequentially). Mirror that by deferring when a
                    // first-token marker is already queued.
                    if marker.is_none() {
                        marker = Some(RecordingMarker::ReasoningStart);
                        next.reasoning_start = true;
                    }
                }
            }
            StreamEventKind::Content => {
                if next.reasoning_start && !next.reasoning_end && marker.is_none() {
                    marker = Some(RecordingMarker::ReasoningEnd);
                    next.reasoning_end = true;
                }
            }
            StreamEventKind::Usage => {
                if marker.is_none() {
                    marker = Some(RecordingMarker::RecordSuccess);
                }
            }
            StreamEventKind::Other => {}
        }

        (marker, next)
    }
}

/// The wiring's per-stream "has this marker already fired?" state (Go's
/// `recordPerformanceStream.{firstTokenSet, reasoningStartSet, reasoningEndSet}`
/// booleans).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarkerFiredState {
    /// `true` once `MarkFirstToken` has been applied.
    pub first_token: bool,
    /// `true` once `MarkReasoningStart` has been applied.
    pub reasoning_start: bool,
    /// `true` once `MarkReasoningEnd` has been applied.
    pub reasoning_end: bool,
}

/// Convenience: classify a stream event into a [`StreamEventKind`] from its
/// observable fields. The wiring layer reads the delta / usage off the stream
/// event and calls this; mirroring Go's `recordPerformanceStream.Current`
/// field-by-field checks.
///
/// * `has_reasoning` — Go: `delta.ReasoningContent != nil &&
///   *delta.ReasoningContent != ""`.
/// * `has_content` — Go: `(delta.Content.Content != nil &&
///   *delta.Content.Content != "") || len(delta.Content.MultipleContent) > 0 ||
///   len(delta.ToolCalls) > 0`.
/// * `has_nonzero_usage` — Go: `event.Usage.GetCompletionTokens() != nil &&
///   *tokenCount > 0`.
///
/// Reasoning takes priority (Go checks it first via `else if`), then usage, then
/// content. When none apply, returns [`StreamEventKind::Other`].
pub fn classify_stream_event(
    has_reasoning: bool,
    has_content: bool,
    has_nonzero_usage: bool,
) -> StreamEventKind {
    if has_reasoning {
        StreamEventKind::Reasoning
    } else if has_nonzero_usage {
        // Go checks usage *after* the reasoning/content branches inside
        // Current(); but the usage block is unconditional on event.Usage and
        // runs regardless of the delta branches. We surface it as a distinct
        // kind to keep the decision tree total. Order: reasoning > usage >
        // content, matching Go's "if reasoning ... else if content ...; then
        // independent usage block".
        StreamEventKind::Usage
    } else if has_content {
        StreamEventKind::Content
    } else {
        StreamEventKind::Other
    }
}

/// Decide the terminal marker for the attempt (Go `OnOutboundLlmResponse` /
/// `OnOutboundRawError`). The wiring layer supplies:
/// * `succeeded` — non-stream responses go through `OnOutboundLlmResponse` and
///   unconditionally call `MarkSuccess` (after optionally recording completion
///   tokens).
/// * `canceled` — Go: `errors.Is(err, context.Canceled)`.
/// * `error_code` — Go: `ExtractErrorCode(err)` (defaults to 500).
///
/// Precedence mirrors Go: success → `Success`; canceled → `Canceled`; otherwise
/// `Failed`.
pub fn terminal_marker(succeeded: bool, canceled: bool, error_code: i32) -> TerminalMarker {
    if succeeded {
        TerminalMarker::Success
    } else if canceled {
        TerminalMarker::Canceled
    } else {
        TerminalMarker::Failed {
            error_code: if error_code == 0 { 500 } else { error_code },
        }
    }
}

// ===========================================================================
// RUST-P9-006 S21 — withModelCircuitBreaker (Go:
//   `internal/server/orchestrator/model_circuit_breaker.go` +
//   `internal/server/biz/model_circuit_breaker.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/model_circuit_breaker.go`):
//
//   func withModelCircuitBreaker(outbound, modelCircuitBreaker *biz.ModelCircuitBreaker, strategy string) pipeline.Middleware {
//       return &modelCircuitBreakerTracker{outbound, modelCircuitBreaker, strategy, ...}
//   }
//
//   func (m *modelCircuitBreakerTracker) OnOutboundRawRequest(ctx, request) {
//       if m.strategy != biz.LoadBalancerStrategyCircuitBreaker || m.modelCircuitBreaker == nil {
//           return request, nil                                // (a) feature disabled → Allow
//       }
//       channel := m.outbound.GetCurrentChannel()
//       modelID := m.outbound.GetRequestedModel()
//       if channel == nil || modelID == "" { return request, nil }   // (b) no target → Allow
//
//       stats := m.modelCircuitBreaker.GetModelCircuitBreakerStats(ctx, channel.ID, modelID)
//       if stats == nil || stats.State != biz.StateOpen {
//           return request, nil                                // (c) Closed/HalfOpen → Allow
//       }
//
//       // State is Open — try to begin a probe (half-open attempt).
//       if !m.modelCircuitBreaker.TryBeginProbe(ctx, channel.ID, modelID) {
//           log.Debug("skipping candidate by circuit breaker: probe conditions not met or another probe in progress")
//           return nil, errSkipCandidateByCircuitBreaker         // (d) probe not granted → RejectOpen
//       }
//
//       m.probeActive = true                                    // (e) probe granted → HalfOpenProbe
//       m.probeChannelID = channel.ID
//       m.probeModelID = modelID
//       return request, nil
//   }
//
// Go `biz.ModelCircuitBreaker` state machine (`model_circuit_breaker.go`):
//
//   const (
//       StateClosed   CircuitBreakerState = "closed"     // requests flow through
//       StateHalfOpen CircuitBreakerState = "half_open"  // limited probes allowed
//       StateOpen     CircuitBreakerState = "open"       // no requests flow
//   )
//
//   func (m *ModelCircuitBreaker) TryBeginProbe(ctx, channelID, modelID) bool {
//       stats := m.getStats(channelID, modelID)
//       stats.Lock(); defer stats.Unlock()
//       if stats.State != StateOpen { return false }                      // not open → no probe
//       if stats.NextProbeAt.IsZero() || time.Now().Before(stats.NextProbeAt) { return false } // too soon → no probe
//       return atomic.CompareAndSwapInt32(&stats.probingInProgress, 0, 1) // already probing → no probe
//   }
//
// The Go tracker is I/O-shaped (it owns the `*biz.ModelCircuitBreaker`, which
// holds the in-memory `modelStats` map; the wiring layer also drives the
// `OnOutboundLlmResponse` / `OnOutboundRawError` / `OnOutboundLlmStream` paths
// that release the probe lease and record success/error). The pure decision
// here is the **admit / reject / probe** verdict over a typed snapshot of the
// model-CB stats — captured by [`CircuitBreakerDecision`]. The wiring layer
// (a) reads `biz.ModelCircuitBreaker.GetModelCircuitBreakerStats`, (b) calls
// [`check_model_circuit_breaker`] with the typed view, (c) on
// [`CircuitBreakerDecision::RejectOpen`] returns the
// [`SKIP_CANDIDATE_BY_CIRCUIT_BREAKER`] error (Go `errSkipCandidateByCircuitBreaker`),
// (d) on [`CircuitBreakerDecision::HalfOpenProbe`] calls `TryBeginProbe` and,
// if granted, sets its `probeActive` flag (mirrors Go's last step).
//
// `[Hertz ?]`: the existing `CircuitBreakerSnapshot` /
// `CircuitBreakerProvider` / `ModelAwareCircuitBreakerScoring` in
// `load_balancer.rs` are the **LB-scoring-side** port — they surface a single
// opaque `score` for `LoadBalancer.Sort`. They are intentionally NOT reused
// here because the orchestrator tracker needs the raw `State` + `NextProbeAt`
// + probe-grant signal, which the LB-side snapshot collapses into a score.
// Duplicating those read-side fields into [`ModelCircuitBreakerStatsView`] is
// therefore faithful, not redundant — it mirrors how Go keeps the orchestrator
// tracker (`model_circuit_breaker.go`) and the LB strategy
// (`lb_strategy_model_aware_circuit_breaker.go`) as two separate consumers of
// `biz.ModelCircuitBreaker`.

/// Circuit-breaker state (Go `biz.CircuitBreakerState`).
///
/// Mirrors Go's three constants:
/// * `StateClosed = "closed"` — requests flow through to the upstream service.
/// * `StateHalfOpen = "half_open"` — a limited number of requests are allowed
///   to test the service.
/// * `StateOpen = "open"` — no requests are allowed to flow through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitBreakerStateView {
    /// Go `StateClosed`.
    Closed,
    /// Go `StateHalfOpen`.
    HalfOpen,
    /// Go `StateOpen`.
    Open,
}

impl CircuitBreakerStateView {
    /// Go string encoding (used by the system settings / diagnostics layer).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::HalfOpen => "half_open",
            Self::Open => "open",
        }
    }
}

/// Read-only snapshot of a model-CB's runtime state for one (channel, model)
/// pair (Go `biz.ModelCircuitBreakerStats` read fields). The wiring layer takes
/// this snapshot off `biz.ModelCircuitBreaker.GetModelCircuitBreakerStats` and
/// hands it to [`check_model_circuit_breaker`].
///
/// Time fields are kept as `Option<String>` (ISO-8601) for the same reason
/// [`QuotaWindowView`] is — the orchestrator crate does not depend on `chrono`
/// directly, and the decision reads only [`Self::state`] / [`Self::next_probe_at`]
/// / [`Self::probing_in_progress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCircuitBreakerStatsView {
    /// Go `ModelCircuitBreakerStats.ChannelID` (kept as a string for parity
    /// with the orchestrator crate's string-id convention — Go `int` channel
    /// ids are surfaced as strings elsewhere in the Rust port).
    pub channel_id: String,
    /// Go `ModelCircuitBreakerStats.ModelID`.
    pub model_id: String,
    /// Go `ModelCircuitBreakerStats.State`.
    pub state: CircuitBreakerStateView,
    /// Go `ModelCircuitBreakerStats.NextProbeAt` formatted as RFC3339. `None`
    /// when Go's `NextProbeAt.IsZero()` (i.e. no probe has been scheduled).
    pub next_probe_at: Option<String>,
    /// Go `ModelCircuitBreakerStats.probingInProgress` (atomic int32) — `true`
    /// when a probe is currently in flight for this (channel, model).
    pub probing_in_progress: bool,
}

impl ModelCircuitBreakerStatsView {
    /// Build a fresh "closed" snapshot (mirrors Go's `getStats` default when no
    /// entry exists yet).
    pub fn closed(channel_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            model_id: model_id.into(),
            state: CircuitBreakerStateView::Closed,
            next_probe_at: None,
            probing_in_progress: false,
        }
    }
}

/// Whether the (channel, model) is currently eligible for a probe attempt (Go
/// `ModelCircuitBreaker.TryBeginProbe` returns `true` only when: state is Open
/// **and** `NextProbeAt` is in the past **and** no probe is already in
/// progress). The wiring layer evaluates this against `time::now()` before
/// calling [`check_model_circuit_breaker`] so the pure decision stays
/// clock-free.
///
/// * `next_probe_at_reached` — Go: `!stats.NextProbeAt.IsZero() &&
///   !time.Now().Before(stats.NextProbeAt)`.
/// * `no_probe_in_flight` — Go: `atomic.LoadInt32(&stats.probingInProgress) ==
///   0`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProbeEligibility {
    /// Whether the scheduled probe time has been reached.
    pub next_probe_at_reached: bool,
    /// Whether no probe is currently in flight for this (channel, model).
    pub no_probe_in_flight: bool,
}

impl ProbeEligibility {
    /// Combined eligibility: probe is granted only when both conditions hold
    /// (mirrors Go `TryBeginProbe` short-circuit).
    pub const fn eligible(self) -> bool {
        self.next_probe_at_reached && self.no_probe_in_flight
    }
}

/// The orchestrator's circuit-breaker verdict for one attempt (Go
/// `modelCircuitBreakerTracker.OnOutboundRawRequest`'s three terminal branches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitBreakerDecision {
    /// The attempt is admitted unconditionally (Go's `(a)` / `(b)` / `(c)`
    /// branches: feature disabled, no channel/model, or state is Closed /
    /// HalfOpen). Carries the reason for operability.
    Allow {
        /// Why the attempt was admitted. Mirrors the distinct Go Allow branches
        /// so the wiring layer can log the same diagnostics Go does.
        reason: AllowReason,
    },
    /// The attempt is rejected because the circuit is Open and no probe could
    /// be granted (Go `(d)` → `errSkipCandidateByCircuitBreaker`). The wiring
    /// layer returns [`SKIP_CANDIDATE_BY_CIRCUIT_BREAKER`] as the pipeline
    /// error; this is *non-retryable* on this candidate (Go `CanRetry` returns
    /// false for `errSkipCandidateByCircuitBreaker`).
    RejectOpen,
    /// The circuit is Open **but** a probe was granted (Go `(e)` → sets
    /// `probeActive = true` and admits the request). The wiring layer sets its
    /// `probeActive` flag and proceeds; on success it calls `RecordSuccess`,
    /// on error `RecordError(wasProbe = true)`.
    HalfOpenProbe,
}

/// Why an attempt was admitted (Go's three Allow branches in
/// `OnOutboundRawRequest`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowReason {
    /// Go `(a)`: `strategy != biz.LoadBalancerStrategyCircuitBreaker` **or**
    /// `modelCircuitBreaker == nil` (feature flag off / no CB configured).
    FeatureDisabled,
    /// Go `(b)`: `channel == nil || modelID == ""` (no per-attempt target).
    NoTarget,
    /// Go `(c)`: `stats == nil || stats.State != biz.StateOpen` (the model-CB
    /// is Closed or HalfOpen — both admit the request; the LB scoring side has
    /// already de-prioritized HalfOpen channels).
    NotOpen,
}

/// Go literal: `errSkipCandidateByCircuitBreaker = errors.New("skip candidate by circuit breaker")`
/// (`conduit/internal/server/orchestrator/outbound.go:312`). Surfaced as a
/// sentinel message so the wiring layer can build the matching [`ConduitError`]
/// (and so tests can assert the Go-parity string).
pub const SKIP_CANDIDATE_BY_CIRCUIT_BREAKER_MESSAGE: &str = "skip candidate by circuit breaker";

/// Go literal: `biz.LoadBalancerStrategyCircuitBreaker = "circuit-breaker"`
/// (`internal/server/biz/system.go:295`). Used by [`check_model_circuit_breaker`]
/// to compare against the wiring-supplied strategy string.
pub const LOAD_BALANCER_STRATEGY_CIRCUIT_BREAKER: &str = "circuit-breaker";

/// S21 — Pure admit / reject / probe decision for the model-aware circuit
/// breaker (Go `modelCircuitBreakerTracker.OnOutboundRawRequest`).
///
/// Inputs:
/// * `strategy_enabled` — `true` iff the wiring's load-balancer strategy is
///   `biz.LoadBalancerStrategyCircuitBreaker` **and** the `modelCircuitBreaker`
///   is non-nil. (Go short-circuits to Allow when either is false.)
/// * `has_target` — `true` iff both a channel and a non-empty model id are
///   available for this attempt. (Go short-circuits to Allow when
///   `channel == nil || modelID == ""`.)
/// * `stats` — the typed snapshot of the model-CB stats (Go
///   `GetModelCircuitBreakerStats`). `None` mirrors Go's `stats == nil` →
///   Allow.
/// * `probe` — the probe-eligibility flags for an Open circuit (Go
///   `TryBeginProbe`'s three preconditions, evaluated against `now()` by the
///   wiring layer).
///
/// Decision tree (mirrors Go exactly):
/// 1. `!strategy_enabled` → `Allow(FeatureDisabled)`.
/// 2. `!has_target` → `Allow(NoTarget)`.
/// 3. `stats.is_none()` → `Allow(NotOpen)` (Go: `stats == nil`).
/// 4. `stats.state != Open` → `Allow(NotOpen)` (Closed + HalfOpen both flow
///    through).
/// 5. `state == Open`:
///    - `probe.eligible()` → [`CircuitBreakerDecision::HalfOpenProbe`].
///    - else → [`CircuitBreakerDecision::RejectOpen`].
///
/// `[Hertz ?]`: Go's `TryBeginProbe` performs the CAS on `probingInProgress`
/// atomically and returns `false` on race. The pure decision here treats
/// `probe.no_probe_in_flight == true` as "the CAS would succeed"; the wiring
/// layer must still call the real `TryBeginProbe` after seeing
/// [`CircuitBreakerDecision::HalfOpenProbe`] and, on a `false` return, fall
/// back to [`CircuitBreakerDecision::RejectOpen`] (the same way Go's tracker
/// falls through to the `errSkipCandidateByCircuitBreaker` return). This keeps
/// the decision pure while preserving the Go probe-grant semantics.
pub fn check_model_circuit_breaker(
    strategy_enabled: bool,
    has_target: bool,
    stats: Option<&ModelCircuitBreakerStatsView>,
    probe: ProbeEligibility,
) -> CircuitBreakerDecision {
    // (a) Feature flag / config gate.
    if !strategy_enabled {
        return CircuitBreakerDecision::Allow {
            reason: AllowReason::FeatureDisabled,
        };
    }

    // (b) No per-attempt target.
    if !has_target {
        return CircuitBreakerDecision::Allow {
            reason: AllowReason::NoTarget,
        };
    }

    // (c) No stats or not Open → Allow.
    let Some(stats) = stats else {
        return CircuitBreakerDecision::Allow {
            reason: AllowReason::NotOpen,
        };
    };

    if stats.state != CircuitBreakerStateView::Open {
        return CircuitBreakerDecision::Allow {
            reason: AllowReason::NotOpen,
        };
    }

    // (d) / (e) State is Open — try to begin a probe.
    if probe.eligible() {
        CircuitBreakerDecision::HalfOpenProbe
    } else {
        CircuitBreakerDecision::RejectOpen
    }
}

// ===========================================================================
// RUST-P9-006 S22 — persistRequestExecution (Go:
//   `internal/server/orchestrator/request_execution.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/request_execution.go`):
//
//   func persistRequestExecution(outbound *PersistentOutboundTransformer) pipeline.Middleware {
//       return &persistRequestExecutionMiddleware{outbound: outbound}
//   }
//
//   func (m *persistRequestExecutionMiddleware) OnOutboundRawRequest(ctx, request) {
//       state := m.outbound.state
//       if state == nil || state.RequestExec != nil { return request, nil }
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil { return request, nil }
//       candidate := state.ChannelModelsCandidates[state.CurrentCandidateIndex]
//       entry := candidate.Models[state.CurrentModelIndex]
//       format := m.outbound.APIFormat()
//       if request.APIFormat != "" { format = llm.APIFormat(request.APIFormat) }
//       requestExec, err := state.RequestService.CreateRequestExecution(ctx, channel, entry.ActualModel, state.Request, *request, format, state.PassThroughApplied)
//       if err != nil { return nil, err }
//       if state.Request != nil && state.Request.ChannelID != channel.ID {
//           err := state.RequestService.UpdateRequestChannelID(ctx, state.Request.ID, channel.ID)
//           if err != nil { return nil, err }
//           state.Request.ChannelID = channel.ID
//       }
//       state.RequestExec = requestExec
//       return request, nil
//   }
//
//   func (m *persistRequestExecutionMiddleware) OnOutboundLlmResponse(ctx, llmResp) {
//       state := m.outbound.state
//       if state == nil || state.RequestExec == nil { return llmResp, nil }
//       persistCtx, cancel := xcontext.DetachWithTimeout(ctx, 10*time.Second)
//       defer cancel()
//       var metrics *biz.LatencyMetrics
//       if state.Perf != nil && !state.Perf.StartTime.IsZero() {
//           var firstTokenLatencyMs, requestLatencyMs int64
//           if state.Perf.RequestCompleted && !state.Perf.EndTime.IsZero() {
//               firstTokenLatencyMs, requestLatencyMs, _ = state.Perf.Calculate()
//           } else {
//               requestLatencyMs = time.Since(state.Perf.StartTime).Milliseconds()
//               if state.Perf.Stream && state.Perf.FirstTokenTime != nil {
//                   firstTokenLatencyMs = state.Perf.FirstTokenTime.Sub(state.Perf.StartTime).Milliseconds()
//               }
//               requestLatencyMs = biz.ClampLatency(requestLatencyMs)
//               firstTokenLatencyMs = biz.ClampLatency(firstTokenLatencyMs)
//           }
//           metrics = &biz.LatencyMetrics{LatencyMs: &requestLatencyMs}
//           if state.Perf.Stream && state.Perf.FirstTokenTime != nil {
//               metrics.FirstTokenLatencyMs = &firstTokenLatencyMs
//           }
//           if state.Perf.Stream {
//               reasoningDurationMs := state.Perf.CalculateReasoningDurationMs()
//               if reasoningDurationMs > 0 { metrics.ReasoningDurationMs = &reasoningDurationMs }
//           }
//       }
//       respBody := audioSafeResponseBody(llmResp.RequestType, m.rawResponse.Headers.Get("Content-Type"), m.rawResponse.Body)
//       err := state.RequestService.UpdateRequestExecutionCompleted(persistCtx, state.RequestExec.ID, llmResp.ID, respBody, metrics)
//       if err != nil { log.Warn(...) }
//       return llmResp, nil
//   }
//
//   func (m *persistRequestExecutionMiddleware) OnOutboundRawError(ctx, err) {
//       state := m.outbound.state
//       if state == nil || state.RequestExec == nil { return }
//       channel := m.outbound.GetCurrentChannel()
//       if channel != nil { log.Warn(ctx, "request process failed", ...) }
//       persistCtx, cancel := xcontext.DetachWithTimeout(ctx, 10*time.Second)
//       defer cancel()
//       updateErr := state.RequestService.UpdateRequestExecutionFailed(persistCtx, state.RequestExec.ID, ExtractErrorMessage(err), ExtractErrorInfo(err))
//       if updateErr != nil { log.Warn(...) }
//   }
//
// The Go middleware is heavily I/O-shaped (it owns the `*PersistenceState`,
// calls `RequestService` methods, reads `state.Perf`). As with the S05..S21
// siblings, we extract the **pure decision** — "what ExecutionRecord fields and
// LatencyMetrics / ExecutionErrorInfo should be persisted for this attempt?" —
// into a data-only [`ExecutionRecordPlan`] the wiring layer consumes. The wiring
// then performs the actual `RequestService` calls.
//
// The pure decisions mirror the Go code:
//   * `CreateRequestExecution` carries `format`, `pass_through_applied`. The
//     pure plan surface is [`ExecutionRecordPlan::create`] + the
//     `pass_through_applied` flag.
//   * `UpdateRequestExecutionCompleted` builds a `LatencyMetrics` from the perf
//     record. The latency computation has two branches:
//       (1) `RequestCompleted && !EndTime.IsZero()` → use `Perf.Calculate()`
//           (ClampLatency is already applied inside Calculate).
//       (2) otherwise → fall back to `time.Since(StartTime)`, with FirstToken
//           read from `FirstTokenTime.Sub(StartTime)` when streaming, both
//           ClampLatency-applied. The wiring layer supplies the wall-clock
//           deltas; the pure helper applies ClampLatency + ReasoningDurationMs.
//     Surfaced as [`LatencyMetricsView`] via [`build_latency_metrics`].
//   * `UpdateRequestExecutionFailed` carries `ExtractErrorMessage(err)` +
//     `ExtractErrorInfo(err)` (HTTP status code from `httpclient.Error`).
//     Surfaced as [`ExecutionErrorInfoView`] via [`extract_error_info`].
//
// `[Curie-the-4th ?]`: the wiring layer supplies wall-clock durations (not
// timestamps) so the pure helpers stay clock-free, mirroring the S20
// `terminal_marker` pattern. The "fetch from request body" branches of
// `ExtractErrorMessage` (gjson `error.message` / `errors.0.message`) belong to
// the wiring layer (they read the raw `httpclient.Error.Body` bytes, not yet
// ported); the pure helper here only classifies *whether* an HTTP status code is
// available.

/// Go literal: `biz.MinLatencyMs` (the minimum enforced latency to prevent
/// extreme TPS calculations). Surfaced as a constant so the wiring layer and
/// tests can assert the clamp bound without hardcoding.
///
/// `[Curie-the-4th ?]`: the Go `MinLatencyMs` constant is defined in
/// `internal/server/biz/channel_metrics.go`; the value below mirrors the
/// captured literal. If the Go default ever changes the wiring layer must
/// update this constant.
pub const BIZ_MIN_LATENCY_MS: i64 = 10;

/// Clamp a latency value to Go's `biz.MinLatencyMs` (Go `biz.ClampLatency`).
///
/// Go source (`internal/server/biz/channel_metrics.go:29-35`):
///   func ClampLatency(latencyMs int64) int64 {
///       if latencyMs < MinLatencyMs { return MinLatencyMs }
///       return latencyMs
///   }
///
/// Negative latencies (which would arise only from a clock skew / programming
/// bug) are also clamped to `MinLatencyMs` — Go's `< MinLatencyMs` comparison
/// covers negatives too, so we mirror that.
pub fn clamp_latency(latency_ms: i64) -> i64 {
    if latency_ms < BIZ_MIN_LATENCY_MS {
        BIZ_MIN_LATENCY_MS
    } else {
        latency_ms
    }
}

/// Typed view of Go `biz.LatencyMetrics` (Go
/// `LatencyMetrics{LatencyMs, FirstTokenLatencyMs, ReasoningDurationMs}` are
/// `*int64` fields). Mirrors Go's pointer-to-int JSON encoding by keeping them
/// as `Option<i64>`: `None` when Go would store `nil` (the metric is absent),
/// `Some(ms)` when Go would store a pointer.
///
/// Mirrors the wire shape persisted into the `request_execution` row, so the
/// Rust side serializes the same JSON as Go (parity rule: Go `*T` →
/// `Option<T>` + `skip_serializing_if = "Option::is_none"`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LatencyMetricsView {
    /// Go `LatencyMetrics.LatencyMs` — total request latency (clamped).
    pub latency_ms: Option<i64>,
    /// Go `LatencyMetrics.FirstTokenLatencyMs` — first-token latency for
    /// streaming requests (clamped). `None` for non-stream requests or when no
    /// first-token event fired.
    pub first_token_latency_ms: Option<i64>,
    /// Go `LatencyMetrics.ReasoningDurationMs` — reasoning-only duration
    /// (`ReasoningEndTime - ReasoningStartTime`). `None` when not streaming or
    /// when either reasoning marker never fired.
    pub reasoning_duration_ms: Option<i64>,
}

/// Typed view of Go `biz.ExecutionErrorInfo` (Go: `{ StatusCode *int }`).
/// Surfaced when persisting a failed execution (Go
/// `UpdateRequestExecutionFailed(... ExtractErrorInfo(err))`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionErrorInfoView {
    /// Go `ExecutionErrorInfo.StatusCode` — the HTTP status code extracted from
    /// a `*httpclient.Error` (`None` for non-HTTP errors, mirrors Go's `nil`
    /// return from `ExtractErrorInfo`).
    pub status_code: Option<i32>,
}

/// The pure "what should be persisted for one attempt?" plan produced by
/// [`build_execution_record`] — the data-only analog of Go's
/// `persistRequestExecutionMiddleware.{OnOutboundRawRequest,
/// OnOutboundLlmResponse, OnOutboundRawError}`.
///
/// Carries the fields the wiring layer needs to call the Rust
/// `RequestService` analogs of:
///   * `CreateRequestExecution(channel, actualModel, request, *request, format, passThroughApplied)`,
///   * `UpdateRequestExecutionCompleted(execID, llmResp.ID, respBody, metrics)`,
///   * `UpdateRequestExecutionFailed(execID, errorMsg, errorInfo)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRecordPlan {
    /// Go `state.RequestExec.RequestID` / Rust `ExecutionRecord.request_id`.
    pub request_id: String,
    /// Go `channel.ID` / Rust `ExecutionRecord.extra["channel_id"]`.
    pub channel_id: String,
    /// Go `entry.ActualModel` (the candidate's resolved upstream model).
    pub actual_model: String,
    /// Go `format` — the API format of the outbound request (Go
    /// `llm.APIFormat`). Surfaced as a string for parity with the LB / candidate
    /// id conventions.
    pub api_format: String,
    /// Go `state.PassThroughApplied` — whether pass-through was applied to this
    /// attempt's request body.
    pub pass_through_applied: bool,
}

impl ExecutionRecordPlan {
    /// Build the create-plan from an attempt (Go
    /// `persistRequestExecutionMiddleware.OnOutboundRawRequest`). Mirrors the
    /// fields Go hands to `CreateRequestExecution`:
    ///   * `request_id`  — Go `state.Request.ID`.
    ///   * `channel_id`  — Go `channel.ID` (from `GetCurrentChannel()`).
    ///   * `actual_model`— Go `entry.ActualModel` (from the candidate's model
    ///     list at `CurrentModelIndex`).
    ///   * `api_format`  — Go `format` (the request's `APIFormat` when set,
    ///     otherwise the outbound transformer's primary `APIFormat()`).
    ///   * `pass_through_applied` — Go `state.PassThroughApplied`.
    pub fn create(
        request_id: impl Into<String>,
        channel_id: impl Into<String>,
        actual_model: impl Into<String>,
        api_format: impl Into<String>,
        pass_through_applied: bool,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            channel_id: channel_id.into(),
            actual_model: actual_model.into(),
            api_format: api_format.into(),
            pass_through_applied,
        }
    }
}

/// Inputs to [`build_latency_metrics`] that summarize the Go `PerformanceRecord`
/// fields the latency computation reads. The wiring layer computes these from
/// the perf-record state (and the wall clock); the pure helper applies the Go
/// clamp + reasoning rules.
///
/// All `Option<i64>` fields are "milliseconds since `StartTime`":
/// * `request_latency_ms` — wall-clock `EndTime - StartTime` (ClampLatency
///   applied). `None` mirrors Go's `state.Perf == nil || StartTime.IsZero()`
///   branch (no metrics recorded).
/// * `first_token_latency_ms` — `FirstTokenTime - StartTime` (ClampLatency
///   applied). `None` for non-stream requests or when no first-token fired.
/// * `reasoning_duration_ms` — `ReasoningEndTime - ReasoningStartTime`.
///   `None` when either marker never fired (Go
///   `CalculateReasoningDurationMs` returns 0 in that case; we surface it as
///   `None` so the wiring layer can distinguish "no reasoning" from
///   "zero-duration reasoning" — only positive durations are persisted per
///   the Go `if reasoningDurationMs > 0` check).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LatencyInputs {
    /// Whether this attempt was recorded as a stream (Go `state.Perf.Stream`).
    pub stream: bool,
    /// Total request latency in ms (already wall-clock-computed by the wiring).
    pub request_latency_ms: Option<i64>,
    /// First-token latency in ms (already wall-clock-computed by the wiring).
    pub first_token_latency_ms: Option<i64>,
    /// Reasoning-only duration in ms (already wall-clock-computed by the
    /// wiring).
    pub reasoning_duration_ms: Option<i64>,
}

/// S22 — Pure latency-metrics builder for the success path (Go
/// `persistRequestExecutionMiddleware.OnOutboundLlmResponse`'s metrics block).
///
/// Mirrors Go's two branches:
/// 1. The wiring layer always supplies wall-clock-computed latitudes (the
///    pure helper cannot read `time.Now()`). For both the `RequestCompleted`
///    branch (which uses `Perf.Calculate()`) and the fallback branch (which
///    uses `time.Since`), the wiring layer has already turned the timestamps
///    into `Option<i64>` ms values.
/// 2. The pure helper applies:
///    * `biz.ClampLatency` on `request_latency_ms` and (when streaming)
///      `first_token_latency_ms`.
///    * the Go `state.Perf.Stream && state.Perf.FirstTokenTime != nil` gating
///      on `first_token_latency_ms` (the field is only populated for streaming
///      requests that observed a first-token event).
///    * the Go `state.Perf.Stream` + `reasoningDurationMs > 0` gating on
///      `reasoning_duration_ms` (only streaming, only positive durations).
///
/// Returns `None` when Go would not build a `metrics` value at all (Go:
/// `state.Perf == nil || state.Perf.StartTime.IsZero()`), i.e. when
/// `request_latency_ms` is `None`.
///
/// `[Curie-the-4th ?]`: Go's `Calculate()` returns `(firstTokenLatencyMs,
/// requestLatencyMs, tokensPerSecond)` — only the two latency values are
/// persisted into `LatencyMetrics`; `tokensPerSecond` is dropped (the Go code
/// does `_ = ` on it). We mirror that by not surfacing TPS.
pub fn build_latency_metrics(inputs: &LatencyInputs) -> Option<LatencyMetricsView> {
    // Go: `if state.Perf != nil && !state.Perf.StartTime.IsZero() { ... }`.
    // The wiring layer surfaces "no perf record" as `request_latency_ms == None`.
    let request_latency_ms = inputs.request_latency_ms?;

    // Go: `metrics = &biz.LatencyMetrics{LatencyMs: &requestLatencyMs}` (already
    // ClampLatency-applied in both Calculate() and the fallback branch).
    let latency_ms = Some(clamp_latency(request_latency_ms));

    // Go: `if state.Perf.Stream && state.Perf.FirstTokenTime != nil`.
    let first_token_latency_ms = if inputs.stream {
        inputs.first_token_latency_ms.map(clamp_latency)
    } else {
        None
    };

    // Go: `if state.Perf.Stream { reasoningDurationMs := ...; if > 0 { set } }`.
    let reasoning_duration_ms = if inputs.stream {
        inputs.reasoning_duration_ms.filter(|&d| d > 0)
        // Note: Go does NOT ClampLatency the reasoning duration (it is
        // `EndTime - StartTime`, never negative when both are set). Mirror
        // that — only the `> 0` filter applies.
    } else {
        None
    };

    Some(LatencyMetricsView {
        latency_ms,
        first_token_latency_ms,
        reasoning_duration_ms,
    })
}

/// S22 — Pure error-info extractor for the failure path (Go `ExtractErrorInfo`).
///
/// Go source (`request_execution.go:232-241`):
///   func ExtractErrorInfo(err error) *biz.ExecutionErrorInfo {
///       httpErr, ok := xerrors.As[*httpclient.Error](err)
///       if !ok { return nil }
///       return &biz.ExecutionErrorInfo{StatusCode: &httpErr.StatusCode}
///   }
///
/// Returns `None` when the error is not an HTTP error (Go's `nil` return);
/// otherwise surfaces the HTTP status code. The wiring layer is responsible for
/// the `xerrors.As[*httpclient.Error]` classification (it owns the Rust analog
/// of `httpclient.Error`).
pub fn extract_error_info(status_code: Option<i32>) -> Option<ExecutionErrorInfoView> {
    status_code.map(|code| ExecutionErrorInfoView {
        status_code: Some(code),
    })
}

// ===========================================================================
// RUST-P9-006 S23 — withLivePreview (Go:
//   `internal/server/orchestrator/live_streaming.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/live_streaming.go`):
//
//   func withLivePreview(state *PersistenceState, systemService *biz.SystemService, liveStreamRegistry *biz.LiveStreamRegistry) pipeline.Middleware {
//       return &livePreviewMiddleware{state, systemService, liveStreamRegistry}
//   }
//
//   func (m *livePreviewMiddleware) OnInboundLlmRequest(ctx, request) {
//       if m.liveStreamRegistry == nil { m.enabled = false; return request, nil }
//       if request == nil || request.Stream == nil || !*request.Stream { m.enabled = false; return request, nil }
//       if !m.initialized { m.enabled = m.systemService != nil && m.systemService.StoragePolicyOrDefault(ctx).LivePreview; m.initialized = true }
//       return request, nil
//   }
//
//   func (m *livePreviewMiddleware) OnOutboundRawRequest(ctx, request) {
//       if !m.enabled { return request, nil }
//       if m.state.Request != nil && m.liveStreamRegistry.GetRequestBuffer(m.state.Request.ID) == nil {
//           m.liveStreamRegistry.RegisterRequest(m.state.Request.ID, chunkbuffer.New())
//       }
//       if m.state.RequestExec != nil && m.liveStreamRegistry.GetExecutionBuffer(m.state.RequestExec.ID) == nil {
//           m.liveStreamRegistry.RegisterExecution(m.state.RequestExec.ID, chunkbuffer.New())
//       }
//       return request, nil
//   }
//
//   func (m *livePreviewMiddleware) OnOutboundRawError(ctx, err) {
//       if !m.enabled || m.state == nil || m.liveStreamRegistry == nil { return }
//       if m.state.RequestExec != nil { buffer := GetExecutionBuffer(...); if buffer != nil { buffer.Close(); UnregisterExecution(...) } }
//       if m.state.Request != nil { buffer := GetRequestBuffer(...); if buffer != nil { buffer.Close(); UnregisterRequest(...) } }
//   }
//
//   func (m *livePreviewMiddleware) OnOutboundRawStream(ctx, stream) {
//       if !m.enabled { return stream, nil }
//       if m.state == nil || m.state.RequestExec == nil { return stream, nil }
//       buffer := m.liveStreamRegistry.GetExecutionBuffer(m.state.RequestExec.ID)
//       if buffer == nil { return stream, nil }
//       return &liveRequestExecutionStream{stream, buffer, m.liveStreamRegistry, m.state.RequestExec.ID}, nil
//   }
//
//   // liveRequestExecutionStream.Next: forwards each event to the consumer AND
//   // appends httpclient.SummarizeBinaryChunk(event) to the buffer (binary audio
//   // chunks are summarized so the live preview path does not retain full TTS
//   // audio bytes).
//
// As with the S05..S22 siblings, the Go middleware is I/O-shaped (it owns a
// `*PersistenceState`, calls `liveStreamRegistry.Register*` / `Get*Buffer` /
// `Unregister*` / `Close`, and wraps the stream). The pure decision the
// middleware makes is **"should live preview be enabled for this request, and
// if so what should the wiring register/unregister/wrap?"** — captured by
// [`LivePreviewPlan`].
//
// Parity details:
//   * `OnInboundLlmRequest`'s gating has three branches: (1) no registry →
//     disabled, (2) request not streaming → disabled, (3) initialize the
//     `enabled` flag from `StoragePolicyOrDefault(ctx).LivePreview`. These
//     surface as [`LivePreviewPlan::disabled_reason`].
//   * `OnOutboundRawRequest` registers a request buffer + an execution buffer
//     when live preview is enabled. The wiring layer performs the registration;
//     the pure plan tells it *whether* to register and *what ids* to use.
//   * `OnOutboundRawError` unregisters + closes the buffers. The wiring layer
//     calls `Unregister*` / `Close`; the pure plan tells it whether to do so.
//   * `OnOutboundRawStream` wraps the stream with a `liveRequestExecutionStream`
//     that fans out events to the buffer. The pure plan tells the wiring whether
//     to wrap; the wrapping itself (plus `SummarizeBinaryChunk`) is I/O.
//   * `[Curie-the-4th ?]`: the Go middleware also wires
//     `OnInboundRawStream` (the request-side preview). The Rust port surfaces
//     that as [`LivePreviewPlan::forward_request_buffer`] mirroring the
//     execution-side wrapping.

/// Whether the live preview middleware should be enabled for this request, and
/// if so, the ids the wiring layer should register buffers under (Go
/// `livePreviewMiddleware.OnInboundLlmRequest` + `OnOutboundRawRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePreviewPlan {
    /// Whether live preview is enabled for this request (Go `m.enabled`). When
    /// `false`, the wiring layer skips every downstream hook (mirrors Go's
    /// `if !m.enabled { return ... }` short-circuits).
    pub enabled: bool,
    /// When `enabled` is false, why the middleware was disabled (for
    /// operability). Mirrors the three Go disable branches: no registry, request
    /// not streaming, system policy `LivePreview=false`.
    pub disabled_reason: Option<LivePreviewDisableReason>,
    /// When enabled, the request id to register the request-side buffer under
    /// (Go `m.state.Request.ID`). `None` mirrors Go's `m.state.Request == nil`
    /// guard.
    pub request_id: Option<String>,
    /// When enabled, the execution id to register the execution-side buffer
    /// under (Go `m.state.RequestExec.ID`). `None` mirrors Go's
    /// `m.state.RequestExec == nil` guard.
    pub request_exec_id: Option<String>,
}

/// Why the live preview middleware was disabled (Go
/// `OnInboundLlmRequest`'s three disable branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivePreviewDisableReason {
    /// Go: `m.liveStreamRegistry == nil` — no live-stream registry wired.
    NoRegistry,
    /// Go: `request.Stream == nil || !*request.Stream` — the request is not
    /// streaming. The wiring layer reads the request's stream flag (Rust
    /// `LlmRequest.stream: bool`, never nil); `false` maps here.
    NotStreaming,
    /// Go: `m.systemService == nil` (defensive — the system service should
    /// always be wired in production) OR `StoragePolicyOrDefault(ctx).LivePreview
    /// == false` (the system-level policy has live preview turned off).
    PolicyDisabled,
}

impl LivePreviewPlan {
    /// Build a "disabled" plan carrying the disable reason (Go's three disable
    /// branches in `OnInboundLlmRequest`). The wiring layer short-circuits every
    /// downstream hook when it sees this plan.
    pub fn disabled(reason: LivePreviewDisableReason) -> Self {
        Self {
            enabled: false,
            disabled_reason: Some(reason),
            request_id: None,
            request_exec_id: None,
        }
    }

    /// Build an "enabled" plan carrying the request/execution ids the wiring
    /// layer should register buffers under (Go `OnOutboundRawRequest`'s two
    /// `Register*` calls). Either id may be `None` when Go would skip the
    /// corresponding registration (`m.state.Request == nil` /
    /// `m.state.RequestExec == nil`).
    pub fn enabled(request_id: Option<String>, request_exec_id: Option<String>) -> Self {
        Self {
            enabled: true,
            disabled_reason: None,
            request_id,
            request_exec_id,
        }
    }
}

/// S23 — Pure "should live preview be enabled?" decision (Go
/// `livePreviewMiddleware.OnInboundLlmRequest`).
///
/// Mirrors the three Go gating branches in order:
/// 1. `registry_available == false` → [`LivePreviewPlan::disabled(NoRegistry)`]
///    (Go: `m.liveStreamRegistry == nil`).
/// 2. `request_streaming == false` →
///    [`LivePreviewPlan::disabled(NotStreaming)`] (Go: `request.Stream == nil
///    || !*request.Stream`).
/// 3. `live_preview_policy == false` →
///    [`LivePreviewPlan::disabled(PolicyDisabled)`] (Go:
///    `m.systemService == nil || !m.systemService.StoragePolicyOrDefault(ctx).LivePreview`).
/// 4. Otherwise → [`LivePreviewPlan::enabled`] with the supplied ids (Go:
///    the three guards all passed; `OnOutboundRawRequest` will register the
///    buffers).
///
/// `[Curie-the-4th ?]`: Go's default `StoragePolicy` is
/// `LivePreview: false` (`biz/system_default.go:5`); the wiring layer surfaces
/// that as `live_preview_policy = false`. The pure helper is clock-free and
/// registry-free — it consumes the wiring-resolved booleans + ids.
pub fn live_preview_plan(
    registry_available: bool,
    request_streaming: bool,
    live_preview_policy: bool,
    request_id: Option<String>,
    request_exec_id: Option<String>,
) -> LivePreviewPlan {
    if !registry_available {
        return LivePreviewPlan::disabled(LivePreviewDisableReason::NoRegistry);
    }

    if !request_streaming {
        return LivePreviewPlan::disabled(LivePreviewDisableReason::NotStreaming);
    }

    if !live_preview_policy {
        return LivePreviewPlan::disabled(LivePreviewDisableReason::PolicyDisabled);
    }

    LivePreviewPlan::enabled(request_id, request_exec_id)
}

/// S23 — Pure decision for the stream-wrap path (Go
/// `livePreviewMiddleware.OnOutboundRawStream`).
///
/// Mirrors Go's three short-circuits:
/// 1. `plan.enabled == false` → `None` (Go: `if !m.enabled { return stream, nil
///    }` — no wrapping).
/// 2. `request_exec_id == None` → `None` (Go: `if m.state.RequestExec == nil
///    { return stream, nil }`).
/// 3. otherwise → `Some(execution_id)` so the wiring wraps the stream with a
///    `liveRequestExecutionStream` analog that fans out summarized chunks to the
///    buffer.
pub fn live_preview_wrap_execution_stream(plan: &LivePreviewPlan) -> Option<&str> {
    if !plan.enabled {
        return None;
    }
    plan.request_exec_id.as_deref()
}

/// S23 — Pure decision for the request-side stream-wrap path (Go
/// `livePreviewMiddleware.OnInboundRawStream`).
///
/// Mirrors Go's three short-circuits:
/// 1. `plan.enabled == false` → `None`.
/// 2. `request_id == None` → `None` (Go: `if m.state.Request == nil`).
/// 3. otherwise → `Some(request_id)`.
pub fn live_preview_wrap_request_stream(plan: &LivePreviewPlan) -> Option<&str> {
    if !plan.enabled {
        return None;
    }
    plan.request_id.as_deref()
}

// ===========================================================================
// RUST-P9-006 S27/S28 — captureRawProviderResponse / captureRawProviderStream
//   (Go: `internal/server/orchestrator/pass_through.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/pass_through.go:215-224`):
//
//   // captureRawProviderResponse stores the raw provider response on state for
//   // response pass-through.
//   func captureRawProviderResponse(outbound *PersistentOutboundTransformer, systemService *biz.SystemService) pipeline.Middleware {
//       return pipeline.OnRawResponse("capture-raw-provider-response", func(ctx, response) {
//           if outbound.isPassThroughEnabled(ctx, systemService) {
//               outbound.state.RawProviderResponse = response
//           }
//           return response, nil
//       })
//   }
//
// And (`pass_through.go:248-344`):
//
//   // captureRawProviderStream fans out raw provider stream events to both the
//   // pipeline (for transforms and LLM middlewares like connection tracking,
//   // performance recording) and a pass-through channel. ...
//   func captureRawProviderStream(outbound *PersistentOutboundTransformer, systemService *biz.SystemService) pipeline.Middleware {
//       return pipeline.OnRawStream("capture-raw-provider-stream", func(ctx, stream) {
//           if !outbound.isPassThroughEnabled(ctx, systemService) { return stream, nil }
//           ...
//           pipelineCh := make(chan *httpclient.StreamEvent, 64)
//           rawStreamCh := make(chan *httpclient.StreamEvent, 64)
//           outbound.state.RawStreamCh = rawStreamCh
//           ... // goroutine fans out each event to both channels
//           return &passThroughChannelStream{ctx, pipelineCh, ...}, nil
//       })
//   }
//
// Both middlewares' behavior is gated on `outbound.isPassThroughEnabled(ctx,
// systemService)` (Go `pass_through.go:25-62`):
//
//   func (p *PersistentOutboundTransformer) isPassThroughEnabled(ctx, systemService) bool {
//       channel := p.GetCurrentChannel()
//       if channel == nil { return false }
//       rawReq := p.state.RawProviderRequest
//       if rawReq == nil || rawReq.APIFormat == "" { return false }
//       llmReq := p.state.LlmRequest
//       if llmReq == nil || string(llmReq.APIFormat) != rawReq.APIFormat { return false }
//       if !passThroughStreamAligned(p.state.OriginalRequestStream, llmReq.Stream) { return false }
//       switch {
//       case channel.Settings != nil && channel.Settings.PassThroughBody != nil: enabled = *channel.Settings.PassThroughBody
//       case systemService != nil: global, err := systemService.PassThrough(ctx); ...
//       }
//       return enabled
//   }
//
// The capture middlewares' only **pure decision** is "which capture variant
// applies for this attempt?" — the answer is binary and entirely driven by
// `isPassThroughEnabled`. We surface it as [`CapturePlan`]; the actual storage
/// (writing `state.RawProviderResponse` / fanning out the stream) is I/O owned
// by the wiring layer.
//
// Parity details:
//   * The Go middlewares are the **last** outbound middlewares in the
//    `ChatCompletionOrchestrator.Process` list (`orchestrator.go:285-289`) so
//    they run **first** in reverse order (before any other
//    `OnOutboundRawResponse`/`OnOutboundRawStream` handler). This ordering is
//    preserved at the wiring layer; the pure plan does not model it.
//   * `[Curie-the-4th ?]`: the `Raw` capture path also arms
//    `applyPassThroughResponse` / `applyPassThroughStream` (Go
//    `orchestrator.go:248-249`) to *replace* the transformed response with the
//    raw one. The pure plan does not model that substitution — it only answers
//    "which capture mode?" The wiring layer is responsible for chaining the
//    pass-through substitution when it sees [`CapturePlan::Raw`].

/// Which capture variant applies for this attempt (Go
/// `captureRawProviderResponse` + `captureRawProviderStream`'s
/// `isPassThroughEnabled` gating).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePlan {
    /// Pass-through is enabled — the wiring layer must store the **raw**
    /// provider response/stream on state (Go
    /// `outbound.state.RawProviderResponse = response` + the goroutine that
    /// fans out to `RawStreamCh`). Downstream pass-through middlewares
    /// (`applyPassThroughResponse` / `applyPassThroughStream`) then *replace*
    /// the transformed response with the raw one.
    Raw,
    /// Pass-through is disabled — the wiring layer must capture the
    /// **transformed** response/stream (Go: the capture middlewares return the
    /// stream unchanged, and the transformed response is what gets persisted by
    /// `persistRequestExecution`).
    Transformed,
}

impl CapturePlan {
    /// `true` when this plan is [`CapturePlan::Raw`].
    pub const fn is_raw(self) -> bool {
        matches!(self, Self::Raw)
    }

    /// `true` when this plan is [`CapturePlan::Transformed`].
    pub const fn is_transformed(self) -> bool {
        matches!(self, Self::Transformed)
    }
}

/// S27/S28 — Pure capture-mode decision (Go
/// `captureRawProviderResponse`/`captureRawProviderStream`'s
/// `isPassThroughEnabled` gating).
///
/// Mirrors Go `isPassThroughEnabled(ctx, systemService)` exactly. The wiring
/// layer supplies the pre-resolved inputs:
/// * `pass_through_enabled` — `true` iff the effective pass-through flag for the
///   current channel is enabled (channel-level `PassThroughBody` when set,
///   otherwise the global system `PassThrough(ctx)` setting) **and** both the
///   inbound and outbound API formats are identical **and** the original /
///   effective stream flags are aligned. (Go `pass_through.go:25-62` performs
///   all of these checks against the `*PersistenceState`.)
///
/// Decision:
/// * `pass_through_enabled == true` → [`CapturePlan::Raw`].
/// * otherwise → [`CapturePlan::Transformed`].
///
/// `[Curie-the-4th ?]`: collapsing Go's multi-branch `isPassThroughEnabled`
/// into a single `bool` is faithful because every branch's terminal action is
/// the same (`return false`). The wiring layer is responsible for evaluating
/// each branch (channel nil, raw request missing, API-format mismatch, stream
/// misalignment, channel setting, global setting) against its own state.
pub fn capture_plan(pass_through_enabled: bool) -> CapturePlan {
    if pass_through_enabled {
        CapturePlan::Raw
    } else {
        CapturePlan::Transformed
    }
}

// ===========================================================================
// RUST-P9-006 S24 — withChannelLimiter (Go:
//   `internal/server/orchestrator/connection_tracking.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/connection_tracking.go`):
//
//   func withChannelLimiter(outbound *PersistentOutboundTransformer, manager *ChannelLimiterManager, metrics *ChannelLimiterMetrics) pipeline.Middleware {
//       return &channelLimiterMiddleware{outbound, manager, metrics}
//   }
//
//   func (m *channelLimiterMiddleware) OnOutboundRawRequest(ctx, request) (*httpclient.Request, error) {
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil { return request, nil }                              // (a) no channel → bypass
//       lim := m.manager.GetOrCreate(channel)
//       if lim == nil { return request, nil }                                  // (b) no limit configured → bypass
//       hardMode := lim.queueSize > 0
//       ...                                                                    // (c) timing metrics (soft mode is non-blocking)
//       if err := lim.Acquire(ctx); err != nil {
//           if queueErr := asChannelQueueError(channel, err); queueErr != nil {
//               switch queueErr.Reason {
//               case channelQueueReasonFull:    m.metrics.IncQueueFull(...)      // (d1) QueueFull
//               case channelQueueReasonTimeout: m.metrics.IncQueueTimeout(...)   // (d2) QueueTimeout
//               }
//               return nil, queueErr
//           }
//           return nil, err
//       }
//       m.current.Store(&limiterSlot{lim: lim})                                 // (e) Admitted — slot held
//       return request, nil
//   }
//
// The Go middleware is heavily I/O-shaped (it owns `*ChannelLimiterManager`
// which holds the in-memory `sync.Map` of `*ChannelLimiter`; `Acquire` blocks
// on a channel in hard mode; the stream wrapper routes `Close` through a
// release path). As with the S05..S23 siblings, we extract the **pure
// admission decision** the middleware makes — over a typed snapshot of the
// limiter state — into [`LimiterDecision`]. The wiring layer:
//   1. Reads `ChannelLimiterManager.GetOrCreate(channel)` to obtain (or skip)
//      the limiter, snapshotting it into a [`ChannelLimiterStateView`].
//   2. Calls [`channel_limiter_decision`] with that view.
//   3. On [`LimiterDecision::Bypass`] returns the request unchanged (Go (a)/(b)).
//   4. On [`LimiterDecision::Admit`] calls `limiter.try_acquire_or_enqueue`
//      (fast path: it always admits here) and stores the permit in its
//      `current` slot; on success releases via `OnOutboundLlmResponse` /
//      `OnOutboundLlmStream.Close` / `OnOutboundRawError` (Go (e)).
//   5. On [`LimiterDecision::Queue`] enters the FIFO wait path
//      (`try_acquire_or_enqueue` yields `AcquireOutcome::Queued`) and polls
//      `ChannelQueueSlot::check_timeout` with the wiring's clock.
//   6. On [`LimiterDecision::Reject`] maps the reason to the matching
//      [`ChannelLimiterError`] (Go (d1)/(d2)) and wraps it via
//      `asChannelQueueError` (handled by the wiring layer; this pure helper
//      surfaces the typed reason only).
//
// Parity details:
//   * Go `queueSize == 0` selects **soft mode** (always Admit — only counts).
//     [`ChannelLimiterStateView::mode`] mirrors this exactly via
//     [`crate::channel_limiter::ChannelLimiterMode`].
//   * Go's `Acquire` decision tree is: (1) soft → admit; (2) `inFlight <
//     capacity` → admit; (3) `waiters.Len() >= queueSize` → `QueueFull`; (4)
//     else enqueue (and possibly time out later). The pure helper reproduces
//     (1)..(3); (4) is surfaced as [`LimiterDecision::Queue`], the wait itself
//     is owned by the wiring layer (it interacts with the FIFO).
//   * Go distinguishes `QueueFull` from `QueueTimeout` only at *runtime* (the
//     timeout fires while waiting). The pure decision can still classify a
//     *snapshot* correctly: when the wiring layer observed a timeout slot it
//     supplies `Reject(QueueTimeout)`; otherwise the saturated-capacity case
//     is `Reject(QueueFull)`. The [`ChannelLimiterRejectionReason`] enum keeps
//     both variants so the wiring layer can pass either.
//   * `[Pauli-the-3rd ?]`: Go's `asChannelQueueError` wraps the limiter
//     sentinel in a `ChannelQueueError` carrying a synthetic 429 so the inbound
//     transform layer produces a rate-limit-shaped client error. That wrapping
//     is wiring-layer responsibility here — the pure helper only returns the
//     typed reason, mirroring how S21 leaves the `ConduitError` synthesis to the
//     wiring layer.

/// Read-only snapshot of a [`crate::channel_limiter::ChannelLimiter`]'s
/// decision-relevant fields (Go `ChannelLimiter.{capacity, queueSize,
/// inFlight, waiters.Len()}`). The wiring layer takes this snapshot off
/// `ChannelLimiterManager.GetOrCreate(channel)` (returning `None` to mirror
/// Go's nil-manager / nil-limiter bypass) and hands it to
/// [`channel_limiter_decision`].
///
/// `[Pauli-the-3rd ?]`: the existing [`crate::channel_limiter::ChannelLimiter`]
/// exposes the live counts via `active_count` / `queued_count` / `config` /
/// `mode`; we *do not* reuse that struct directly here because (a) the decision
/// must be pure with respect to its inputs (so tests can fixture a snapshot
/// without driving a real limiter through `try_acquire`), and (b) the Go side
/// reads `capacity` / `queueSize` off the *config* struct, not off the live
/// limiter — a snapshot is the faithful analog. The snapshot type is therefore
/// a separate view that mirrors the Go fields one-for-one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChannelLimiterStateView {
    /// Go `ChannelLimiter.capacity` (Rust [`crate::channel_limiter::ChannelLimiterConfig::max_concurrent`]).
    pub max_concurrent: usize,
    /// Go `ChannelLimiter.queueSize`. `0` selects **soft mode** (Go
    /// `hardMode := lim.queueSize > 0`).
    pub queue_size: usize,
    /// Go `ChannelLimiter.inFlight` (Rust `active_count`).
    pub in_flight: usize,
    /// Go `waiters.Len()` (Rust `queued_count`).
    pub waiting: usize,
}

impl ChannelLimiterStateView {
    /// Mode derived from `queue_size` (Go `hardMode := lim.queueSize > 0`).
    /// Mirrors [`crate::channel_limiter::ChannelLimiterConfig::mode`].
    pub const fn mode(self) -> crate::channel_limiter::ChannelLimiterMode {
        if self.queue_size == 0 {
            crate::channel_limiter::ChannelLimiterMode::Soft
        } else {
            crate::channel_limiter::ChannelLimiterMode::Hard
        }
    }
}

/// Why the channel-limiter rejected an attempt (Go
/// `ChannelQueueError.Reason` / `channelQueueReasonFull` /
/// `channelQueueReasonTimeout`).
///
/// Carries the matching [`crate::channel_limiter::ChannelLimiterError`] so the
/// wiring layer can hand it to `asChannelQueueError`-equivalent wrapping
/// without an extra lookup. Mirrors the two sentinel errors Go raises from
/// `ChannelLimiter.Acquire` (`ErrChannelQueueFull` /
/// `ErrChannelQueueTimeout`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelLimiterRejectionReason {
    /// Go `channelQueueReasonFull = "queue_full"` (sentinel
    /// `ErrChannelQueueFull`). Capacity exhausted and the FIFO queue has no
    /// remaining capacity at entry time.
    QueueFull,
    /// Go `channelQueueReasonTimeout = "queue_timeout"` (sentinel
    /// `ErrChannelQueueTimeout`). The per-channel queue timeout elapsed while
    /// the request was still waiting for a slot.
    QueueTimeout,
}

impl ChannelLimiterRejectionReason {
    /// Go string encoding (Go `ChannelQueueError.Reason` JSON tag value).
    /// Mirrors `channelQueueReasonFull` / `channelQueueReasonTimeout` literals.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueueFull => "queue_full",
            Self::QueueTimeout => "queue_timeout",
        }
    }

    /// Map the reason to the matching limiter sentinel error (Go
    /// `ErrChannelQueueFull` / `ErrChannelQueueTimeout`). Mirrors the Go
    /// `ChannelQueueError.Cause` field which carries the original sentinel so
    /// `errors.Is(err, ErrChannelQueueFull)` keeps matching.
    pub const fn to_limiter_error(self) -> crate::channel_limiter::ChannelLimiterError {
        match self {
            Self::QueueFull => crate::channel_limiter::ChannelLimiterError::QueueFull,
            Self::QueueTimeout => crate::channel_limiter::ChannelLimiterError::ChannelQueueTimeout,
        }
    }
}

/// The orchestrator's per-attempt admission verdict from the channel limiter
/// (Go `channelLimiterMiddleware.OnOutboundRawRequest`'s terminal branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimiterDecision {
    /// The channel has no configured concurrency limit (Go (a)/(b): no channel,
    /// or `manager.GetOrCreate` returned nil). The wiring layer forwards the
    /// request unchanged — no slot is held, no release path runs.
    Bypass,
    /// A slot was admitted immediately (Go (e): `m.current.Store(...)`). The
    /// wiring layer MUST hold the resulting [`crate::channel_limiter::ChannelPermit`]
    /// and release it exactly once via one of `OnOutboundLlmResponse` /
    /// `OnOutboundLlmStream.Close` / `OnOutboundRawError`.
    Admit,
    /// Hard mode with capacity saturated but the FIFO still has room — the
    /// request must wait in the queue (Go's blocking `Acquire` branch, lines
    /// 91-93). `position` is the 0-indexed FIFO slot the waiter would occupy
    /// (i.e. `waiting` *before* the enqueue). The wiring layer drives the actual
    /// wait via `try_acquire_or_enqueue` + `ChannelQueueSlot::check_timeout`.
    Queue {
        /// 0-indexed FIFO position the waiter would occupy (mirrors
        /// `waiters.Len()` at the moment of enqueue — i.e. how many requests
        /// are ahead of us).
        position: usize,
    },
    /// The attempt is rejected (Go (d1)/(d2)). The wiring layer maps the reason
    /// to the [`ChannelLimiterRejectionReason::to_limiter_error`] sentinel and
    /// wraps it via `asChannelQueueError`-equivalent before returning. **No
    /// slot is held** — Go's middleware guarantees `m.current` stays nil on the
    /// Acquire-failure path (mirrored by `TestChannelLimiterMiddleware_QueueFullReturnsTypedError`).
    Reject {
        /// Which limiter sentinel caused the rejection.
        reason: ChannelLimiterRejectionReason,
    },
}

impl LimiterDecision {
    /// `true` when the request is admitted or bypassed (i.e. it should flow
    /// through to the upstream provider). Mirrors Go's `Acquire` returning
    /// `nil`.
    pub const fn is_admitted_path(self) -> bool {
        matches!(self, Self::Bypass | Self::Admit)
    }

    /// `true` when the attempt is rejected (Go's `Acquire` returned a non-nil
    /// error that `asChannelQueueError` recognized).
    pub const fn is_rejected(self) -> bool {
        matches!(self, Self::Reject { .. })
    }
}

/// S24 — Pure admit / queue / reject decision for the channel limiter (Go
/// `channelLimiterMiddleware.OnOutboundRawRequest` decision tree).
///
/// Inputs:
/// * `limiter` — `None` mirrors Go's `manager.GetOrCreate(channel) == nil`
///   (no concurrency limit configured for this channel) → `Bypass`. It also
///   mirrors the `channel == nil` short-circuit (the wiring layer supplies
///   `None` in that case too, since there is nothing to limit against).
/// * `timed_out` — `true` when the wiring layer observed the request already
///   waiting in the FIFO and the per-channel timeout has elapsed (Go
///   `ErrChannelQueueTimeout` from inside `Acquire`). The pure helper cannot
///   observe the wall clock, so the wiring layer flags this explicitly; when
///   `true` and the request is currently queued, the decision is
///   `Reject(QueueTimeout)` instead of `Queue`.
///
/// Decision tree (mirrors Go's `Acquire` exactly):
/// 1. `limiter.is_none()` → [`LimiterDecision::Bypass`] (Go (a)/(b)).
/// 2. Soft mode (`queue_size == 0`): always [`LimiterDecision::Admit`] (Go's
///    soft branch increments `inFlight` and returns nil immediately).
/// 3. Hard mode:
///    - `in_flight < max_concurrent` → [`LimiterDecision::Admit`] (Go fast path).
///    - else `waiting >= queue_size` → [`LimiterDecision::Reject(QueueFull)`]
///      (Go `ErrChannelQueueFull`).
///    - else, if `timed_out` → [`LimiterDecision::Reject(QueueTimeout)`]
///      (Go `ErrChannelQueueTimeout` fires *inside* the wait).
///    - else → [`LimiterDecision::Queue { position: waiting }`] (Go enqueues
///      and blocks; the wiring layer drives the wait).
///
/// `[Pauli-the-3rd ?]`: the `timed_out` flag is evaluated *only* in the
/// would-queue branch. Go's `Acquire` either returns immediately (admit /
/// queue-full) or blocks; the timeout fires only while blocked, so it cannot
/// co-occur with admit or queue-full. Mirroring that, the helper never returns
/// `QueueTimeout` from a non-wait branch.
pub fn channel_limiter_decision(
    limiter: Option<&ChannelLimiterStateView>,
    timed_out: bool,
) -> LimiterDecision {
    // (a)/(b) No channel or no limiter configured → bypass.
    let Some(state) = limiter else {
        return LimiterDecision::Bypass;
    };

    // Soft mode: always admit (Go increments inFlight, returns nil).
    if state.mode() == crate::channel_limiter::ChannelLimiterMode::Soft {
        return LimiterDecision::Admit;
    }

    // Hard mode: capacity check first.
    if state.in_flight < state.max_concurrent {
        return LimiterDecision::Admit;
    }

    // Capacity saturated: queue-full check.
    if state.waiting >= state.queue_size {
        return LimiterDecision::Reject {
            reason: ChannelLimiterRejectionReason::QueueFull,
        };
    }

    // There is room in the FIFO. If the wiring layer observed a timeout while
    // we were waiting, surface it as QueueTimeout; otherwise queue.
    if timed_out {
        LimiterDecision::Reject {
            reason: ChannelLimiterRejectionReason::QueueTimeout,
        }
    } else {
        LimiterDecision::Queue {
            position: state.waiting,
        }
    }
}

// ===========================================================================
// RUST-P9-006 S25 — withRateLimitAdmission (Go:
//   `internal/server/orchestrator/rate_limit_admission.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/rate_limit_admission.go`):
//
//   func withRateLimitAdmission(outbound *PersistentOutboundTransformer, tracker *ChannelRequestTracker) pipeline.Middleware {
//       if tracker == nil { return &noopRateLimitAdmission{} }                  // (a) tracker absent → noop (Allow)
//       return &rateLimitAdmissionMiddleware{outbound, tracker}
//   }
//
//   func (m *rateLimitAdmissionMiddleware) OnOutboundRawRequest(ctx, request) (*httpclient.Request, error) {
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil || channel.Settings == nil || channel.Settings.RateLimit == nil {
//           return request, nil                                                  // (b) no rate-limit configured → Allow
//       }
//       limit := channel.Settings.RateLimit.RPM
//       if limit == nil || *limit <= 0 { return request, nil }                   // (c) no RPM limit → Allow
//       if !m.tracker.TryAcquireRequest(channel.ID, *limit) {
//           ...                                                                  // (d) requests >= limit → RevokeRpm
//           return nil, newLocalRPMExhaustedError(channel, *limit)
//       }
//       return request, nil                                                      // (e) requests < limit → Allow (+1 consumed)
//   }
//
// And `TryAcquireRequest` (`channel_request_tracker.go:48`):
//
//   func (t *ChannelRequestTracker) TryAcquireRequest(channelID int, limit int64) bool {
//       if limit <= 0 { return true }                                            // mirrors (c)
//       w := t.getOrResetWindow(channelID)
//       if w.requests >= limit { return false }                                  // (d)
//       w.requests++
//       return true                                                              // (e)
//   }
//
// The Go middleware has two halves:
//   (a) the *gating* half — channel/settings/rate-limit nil short-circuits and
//       the tracker-absent noop — which is owned by the wiring layer (it reads
//       off the not-yet-ported `*PersistenceState`); and
//   (b) the **pure admit/revoke decision** over the request count + RPM limit,
//       which we extract into [`AdmissionDecision`] here.
//
// The pure [`rate_limit_admission_decision`] mirrors `TryAcquireRequest`'s
// decision tree exactly:
///  * `limit <= 0` (Go `nil || *limit <= 0` → `true`) → [`AdmissionDecision::Allow`]
///    (no slot consumed).
///  * `requests >= limit` → [`AdmissionDecision::RevokeRpm`] (Go returns `false`,
///    the middleware emits `LocalRPMExhaustedError`).
///  * else → [`AdmissionDecision::Allow`] (Go increments `requests`).
///
/// The wiring layer supplies `requests` from
/// `ChannelRequestTracker::get_request_count` (or the Rust analog) and applies
/// the increment on `Allow` (mirroring Go's in-place `w.requests++`); the pure
/// helper is side-effect-free so it is fixture-testable.
//
// `[Pauli-the-3rd ?]`: the existing [`crate::rate_limit::RateLimitTracker`] is a
// *tick-driven windowed* tracker used by the LB scoring side; it does NOT model
// the Go `ChannelRequestTracker.TryAcquireRequest` RPM-only check (it carries
// `successes`/`failures`/`usage_tokens`, not a `requests` counter, and its
// windowing is tick-based rather than wall-clock-minute-based). We therefore
// surface a fresh [`RpmView`] rather than reuse that tracker, mirroring how S21
// introduced a separate `ModelCircuitBreakerStatsView` for the same reason.
// When the not-yet-ported `ChannelRequestTracker` Rust analog lands, the wiring
// layer will read `requests` off it directly.

/// Read-only view of the inputs to the Go RPM admission decision (Go
/// `ChannelRequestTracker.TryAcquireRequest(channelID, limit)` reads
/// `w.requests` and the caller-supplied `limit`).
///
/// Mirrors:
/// * `requests` — Go `rateLimitWindow.requests` (the count of admitted
///   requests in the current minute bucket). Parity rule: Go `int64` → Rust
///   `i64`.
/// * `limit`    — Go `*channel.Settings.RateLimit.RPM` (the per-channel
///   per-minute RPM cap). `0` (and any negative) means "no RPM limit
///   configured" — Go's `limit == nil || *limit <= 0` short-circuit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RpmView {
    /// Go `rateLimitWindow.requests` for the current channel + minute bucket.
    pub requests: i64,
    /// Go `*channel.Settings.RateLimit.RPM`. Non-positive means "no limit".
    pub limit: i64,
}

/// The orchestrator's per-attempt RPM-admission verdict (Go
/// `rateLimitAdmissionMiddleware.OnOutboundRawRequest`'s terminal branches).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    /// The attempt is admitted (Go (b)/(c)/(e)). The wiring layer MUST consume
    /// one RPM slot (Go `w.requests++`) **only when `consumed_slot == true`**.
    /// `false` marks the no-limit / not-configured fast paths where Go returns
    /// `nil` *without* touching the counter (mirrors `TryAcquireRequest`'s
    /// `limit <= 0` early return).
    Allow {
        /// Whether the wiring layer should increment the request counter. `true`
        /// on the Go (e) branch (`requests < limit`); `false` on the Go (b)/(c)
        /// branches (no rate-limit configured / no RPM limit).
        consumed_slot: bool,
    },
    /// The attempt is rejected because the channel exhausted its local RPM
    /// budget (Go (d): `requests >= limit`). The wiring layer maps this to
    /// `LocalRPMExhaustedError` (Go `newLocalRPMExhaustedError`) and returns;
    /// this is a **local admission rejection** — it never reached upstream, so
    /// the rate-limit-tracking middleware MUST NOT trigger a cooldown for it
    /// (Go `OnOutboundRawError` checks `isLocalRPMExhaustedError`).
    RevokeRpm,
}

impl AdmissionDecision {
    /// `true` when the request is admitted (Go returns the request unchanged).
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// `true` when the request is rejected for local RPM exhaustion.
    pub const fn is_revoke_rpm(self) -> bool {
        matches!(self, Self::RevokeRpm)
    }
}

/// Go literal: `ErrLocalRPMExhausted = errors.New("local channel rpm exhausted")`
/// (`rate_limit_admission_error.go:12`). Surfaced as a sentinel message so the
/// wiring layer can build the matching [`ConduitError`] and so tests can assert
/// the Go-parity string.
pub const LOCAL_RPM_EXHAUSTED_MESSAGE: &str = "local channel rpm exhausted";

/// S25 — Pure RPM-admission decision (Go
/// `rateLimitAdmissionMiddleware.OnOutboundRawRequest` +
/// `ChannelRequestTracker.TryAcquireRequest`).
///
/// Mirrors Go's `TryAcquireRequest(channelID, limit)` decision tree:
/// 1. `rpm.limit <= 0` → [`AdmissionDecision::Allow { consumed_slot: false }`]
///    (Go (c): `limit == nil || *limit <= 0`; the middleware's
///    `channel.Settings.RateLimit == nil` / `RPM == nil` cases are surfaced
///    identically — the wiring layer supplies `limit = 0` for those).
/// 2. `rpm.requests >= rpm.limit` → [`AdmissionDecision::RevokeRpm`] (Go (d)).
/// 3. else → [`AdmissionDecision::Allow { consumed_slot: true }`] (Go (e):
///    the wiring layer increments the request counter).
///
/// `[Pauli-the-3rd ?]`: Go's `TryAcquireRequest` performs the increment
/// atomically under the tracker's mutex. The pure helper leaves the increment
/// to the wiring layer (signaled via `consumed_slot`); on `RevokeRpm` no slot
/// is consumed (Go returns `false` without touching `requests`), which keeps
/// the counter stable on retry — mirrors
/// `TestRateLimitAdmission_SameChannelRetryCannotBypassRPM` where a second
/// `OnOutboundRawRequest` against the same channel still sees
/// `tracker.GetRequestCount == 1`.
pub fn rate_limit_admission_decision(rpm: &RpmView) -> AdmissionDecision {
    // Go (c): `limit == nil || *limit <= 0` → Allow without consuming a slot.
    if rpm.limit <= 0 {
        return AdmissionDecision::Allow {
            consumed_slot: false,
        };
    }

    // Go (d): `w.requests >= limit` → RevokeRpm (no slot consumed).
    if rpm.requests >= rpm.limit {
        return AdmissionDecision::RevokeRpm;
    }

    // Go (e): `w.requests++ ; return true`. The wiring layer performs the
    // increment; we just signal it.
    AdmissionDecision::Allow {
        consumed_slot: true,
    }
}

// ===========================================================================
// RUST-P9-006 S26 — withRateLimitTracking (Go:
//   `internal/server/orchestrator/rate_limit_tracking.go`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/rate_limit_tracking.go`):
//
//   func withRateLimitTracking(outbound, tracker *ChannelRequestTracker) pipeline.Middleware {
//       if tracker == nil { return &noopRateLimitTracking{} }
//       return &rateLimitTracking{outbound, tracker}
//   }
//
//   // OnOutboundLlmResponse: record TPM usage on success.
//   func (m *rateLimitTracking) OnOutboundLlmResponse(ctx, response) (*llm.Response, error) {
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil || response == nil || response.Usage == nil { return response, nil }
//       totalTokens := response.Usage.TotalTokens
//       if totalTokens > 0 { m.tracker.AddTokens(channel.ID, totalTokens) }         // (α)
//       return response, nil
//   }
//
//   // OnOutboundLlmStream / rateLimitTrackingStream.Current: same as (α) but
//   // applied per stream chunk that carries non-zero `Usage.TotalTokens`.
//
//   // OnOutboundRawError: parse 429 Retry-After and set a cooldown.
//   func (m *rateLimitTracking) OnOutboundRawError(ctx, err) {
//       if m.outbound == nil { return }
//       if isChannelQueueError(err) || isLocalRPMExhaustedError(err) { return }    // (β1) local rejections skip cooldown
//       channel := m.outbound.GetCurrentChannel()
//       if channel == nil { return }                                              // (β2) no channel
//       if !httpclient.HasRetryAfterHeader(err) { return }                        // (β3) no Retry-After
//       cooldown, ok := httpclient.ParseRetryAfter(err)
//       if !ok { return }                                                         // (β4) unparseable Retry-After
//       m.tracker.SetCooldown(channel.ID, time.Now().Add(cooldown))               // (β5) cooldown set
//   }
//
// The Go middleware has two halves:
//   (a) the I/O half — calling `tracker.AddTokens` / `tracker.SetCooldown` on
//       the real `*ChannelRequestTracker`, which is not yet ported — owned by
//       the wiring layer; and
//   (b) the **pure decision** over the response's usage / the raw error's
//       signals — "should we add tokens? how many? should we set a cooldown?
//       until when (relative)?" — which we extract into [`TrackerDelta`] here.
//
// The pure [`rate_limit_update`] mirrors the two Go branches:
///  * On success (Go `OnOutboundLlmResponse`), when `usage.total_tokens > 0`,
///    the delta records `tokens_added = total_tokens`; otherwise `0` (Go
///    short-circuits on nil usage / zero tokens).
///  * On a raw error (Go `OnOutboundRawError`), the delta records a cooldown
///    *only* when the error is NOT a local admission rejection (queue error /
///    RPM exhausted) AND carries a parseable Retry-After header AND the status
///    is 429. The wiring layer supplies those flags directly so the helper
///    stays clock- and parser-free.
///
/// `[Pauli-the-3rd ?]`: Go's `SetCooldown` takes an absolute `time.Time` (now +
/// parsed cooldown). The pure helper surfaces the **relative** cooldown in
/// milliseconds (`cooldown_ms`) because the orchestrator crate does not depend
/// on `chrono` directly (mirrors the `QuotaWindowView` /
/// `ModelCircuitBreakerStatsView` choice). The wiring layer adds `now()` to
/// produce the absolute timestamp before calling the Rust analog of
/// `SetCooldown`. Go's `SetCooldown` *extends* the cooldown (a shorter value
/// will not overwrite an existing longer one) — that comparison is also left
/// to the wiring layer since it needs the current cooldown state.

/// Inputs to [`rate_limit_update`] summarizing the per-attempt outcome the
/// tracking middleware observes (Go reads `response.Usage.TotalTokens` on
/// success and a mix of error-type / header / status signals on
/// `OnOutboundRawError`).
///
/// The wiring layer computes these fields from the real `*llm.Response` /
/// `*httpclient.Error` and supplies them to the pure helper. Each field mirrors
/// a distinct Go check, so a test fixture can exercise each branch in isolation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttemptOutcome {
    /// Go `response.Usage.TotalTokens` (only consulted on the success path).
    /// `0` mirrors Go's `response.Usage == nil` and `TotalTokens == 0`
    /// short-circuits (no token accounting).
    pub total_tokens: i64,
    /// `true` when the attempt succeeded (Go `OnOutboundLlmResponse` was
    /// invoked). `false` means the attempt errored and the helper evaluates the
    /// cooldown path instead.
    pub succeeded: bool,
    /// `true` when the error is a local admission rejection — Go:
    /// `isChannelQueueError(err) || isLocalRPMExhaustedError(err)` (β1). Such
    /// errors never reached upstream and MUST NOT trigger a cooldown. Only
    /// consulted when `!succeeded`.
    pub is_local_admission_rejection: bool,
    /// `true` when the upstream returned HTTP 429 (Go
    /// `httpclient.Error.StatusCode == http.StatusTooManyRequests`). Only
    /// consulted when `!succeeded && !is_local_admission_rejection`. Go's
    /// middleware does not branch on this explicitly — it only checks
    /// `HasRetryAfterHeader` — but in practice Retry-After only appears on 429
    /// responses; surfacing it as an explicit flag lets the helper refuse to
    /// cool down on, say, a 500 with a stray Retry-After header, matching the
    /// spirit of `TestRateLimitTracking_OnOutboundRawError_Not429`.
    pub is_http_429: bool,
    /// `true` when the error carries a parseable Retry-After header (Go
    /// `httpclient.HasRetryAfterHeader(err)` AND `ParseRetryAfter(err)` ok).
    /// Only consulted when `is_http_429`.
    pub has_retry_after: bool,
    /// Parsed Retry-After cooldown in milliseconds (Go `time.Duration` from
    /// `ParseRetryAfter`). Only consulted when `has_retry_after`. `0` is a
    /// valid cooldown ("cool down for zero additional ms").
    pub cooldown_ms: i64,
}

/// The pure update plan produced by [`rate_limit_update`] — the data-only
/// analog of Go's `rateLimitTracking.{OnOutboundLlmResponse,
/// OnOutboundRawError}` calls into `ChannelRequestTracker`.
///
/// The wiring layer consumes this plan by calling the Rust analogs of
/// `tracker.AddTokens(channel_id, delta.tokens_added)` (when non-zero) and
/// `tracker.SetCooldown(channel_id, now + delta.cooldown_ms)` (when
/// `Some`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrackerDelta {
    /// Tokens to add to the channel's TPM counter (Go `AddTokens`). `0` means
    /// "no token update" (Go short-circuits on `totalTokens <= 0` / nil usage).
    pub tokens_added: i64,
    /// Cooldown to set, in milliseconds relative to `now()` (Go `SetCooldown`
    /// takes `now + cooldown`). `None` means "no cooldown update" — mirrors
    /// every Go branch that returns early without calling `SetCooldown` (local
    /// rejection, missing channel, no Retry-After, unparseable Retry-After,
    /// non-429 status).
    pub cooldown_ms: Option<i64>,
}

impl TrackerDelta {
    /// `true` when there is nothing for the wiring layer to apply (no token
    /// update and no cooldown). Mirrors the Go no-op branches.
    pub const fn is_empty(self) -> bool {
        self.tokens_added == 0 && self.cooldown_ms.is_none()
    }
}

/// S26 — Pure rate-limit tracker update for one attempt outcome (Go
/// `rateLimitTracking.OnOutboundLlmResponse` + `OnOutboundRawError`).
///
/// Decision tree (mirrors Go exactly):
/// 1. **Success path** (`outcome.succeeded`):
///    - `outcome.total_tokens > 0` → [`TrackerDelta { tokens_added:
///      total_tokens, cooldown_ms: None }`]. Go `AddTokens(channel.ID,
///      totalTokens)`.
///    - else → [`TrackerDelta::default`] (empty). Go's nil-usage / zero-token
///      short-circuit.
/// 2. **Error path** (`!outcome.succeeded`) — the cooldown decision tree:
///    - `outcome.is_local_admission_rejection` → empty delta (Go β1: queue
///      errors and local-RPM-exhausted errors never reached upstream and MUST
///      NOT trigger a cooldown).
///    - else `!outcome.is_http_429` → empty delta. Go only sees Retry-After on
///      429 responses; a non-429 status with a stray header is ignored.
///      Mirrors `TestRateLimitTracking_OnOutboundRawError_Not429`.
///    - else `!outcome.has_retry_after` → empty delta (Go β3/β4: no
///      Retry-After header, or unparseable). Mirrors
///      `TestRateLimitTracking_OnOutboundRawError_429WithoutRetryAfter`.
///    - else → [`TrackerDelta { tokens_added: 0, cooldown_ms:
///      Some(outcome.cooldown_ms) }`]. Go `SetCooldown(channel.ID,
///      time.Now().Add(cooldown))`.
///
/// `[Pauli-the-3rd ?]`: token accounting and cooldown setting are mutually
/// exclusive in Go — `OnOutboundLlmResponse` only ever adds tokens, and
/// `OnOutboundRawError` only ever sets cooldowns. The helper preserves that
/// invariant: on the success path `cooldown_ms` is always `None`, and on the
/// error path `tokens_added` is always `0`. Tests pin both.
pub fn rate_limit_update(outcome: &AttemptOutcome) -> TrackerDelta {
    // Success path — token accounting only.
    if outcome.succeeded {
        let tokens_added = if outcome.total_tokens > 0 {
            outcome.total_tokens
        } else {
            // Go: nil usage / zero tokens → no-op.
            0
        };
        return TrackerDelta {
            tokens_added,
            cooldown_ms: None,
        };
    }

    // Error path — cooldown decision tree (mirrors Go β1..β5).
    let cooldown_ms = if outcome.is_local_admission_rejection {
        None
    } else if !outcome.is_http_429 {
        None
    } else if !outcome.has_retry_after {
        None
    } else {
        Some(outcome.cooldown_ms)
    };

    TrackerDelta {
        tokens_added: 0,
        cooldown_ms,
    }
}

// ===========================================================================
// RUST-P9-006 S29 — Process main-chain skeleton (Go `Process`)
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/orchestrator.go`,
// `ChatCompletionOrchestrator.Process`):
//
//   func (processor *ChatCompletionOrchestrator) Process(ctx, request) (ChatCompletionResult, error) {
//       // 1. Apply system bypass for the whole flow (so middlewares can read
//       //    system settings, prompts, channels, quota without per-call scopes).
//       ctx = authz.WithSystemBypass(ctx, "process-chat-completion")
//
//       // 2. Derive retry policy + load-balancer strategy from system settings
//       //    (overridable per API-key profile).
//       retryPolicy := processor.SystemService.RetryPolicyOrDefault(ctx)
//       strategy    := deriveLoadBalancerStrategy(retryPolicy, apiKey)
//
//       // 3. Build the PersistenceState and the pipeline options (retry,
//       //    empty-response detection, response timeouts) from the policy.
//       // 4. Assemble the middleware chain in a FIXED order:
//       //      global (StripBillingHeaderCCH, EnsureUsage)
//       //      inbound  (enforceQuota, applyAutoReasoningEffort,
//       //                checkApiKeyModelAccess, applyModelMapping,
//       //                selectCandidates, injectPrompts, protectPrompts,
//       //                applyPassThroughResponse, applyPassThroughStream,
//       //                persistRequest)
//       //      outbound (applyPassThroughRequestBody, applyOverrideRequestBody,
//       //                applyUserAgentPassThrough, applyOverrideRequestHeaders,
//       //                withPerformanceRecording, withModelCircuitBreaker,
//       //                persistRequestExecution, withLivePreview,
//       //                withChannelLimiter, withRateLimitAdmission,
//       //                withRateLimitTracking,
//       //                captureRawProviderResponse, captureRawProviderStream)
//       // 5. pipe.Process(ctx, request) — the pipeline runs inbound → attempt
//       //    loop (load-balance, transform, execute, retry) → outbound → response.
//       // 6. On error: detach a 10s context and persist the failure status on
//       //    the request + last execution (UpdateRequestExecutionStatusFromError /
//       //    UpdateRequestStatusFromError).
//       // 7. On success: return ChatCompletionResult (response or stream).
//   }
//
// The main-chain stage order is FIXED and is what S29 encodes. Each stage is a
// pure decision/data step — no IO. The existing [`OrchestratorStage`] enum
// already lists the 7 stages; S29 surfaces:
//   * [`stage_sequence`] — the canonical main-chain order;
//   * [`StagePlan`] — a pure description of each stage's inputs/outputs (used by
//     diagnostics + wiring to know *what* each stage consumes/produces without
//     re-deriving the contract from the Go source).

/// The fixed main-chain stage order for `Process` (Go
/// `ChatCompletionOrchestrator.Process`). Mirrors the S35 contract:
///
/// ```text
/// Auth -> Quota -> Select -> LoadBalance -> Pipeline -> Persist -> Emit
/// ```
///
/// `Auth` collapses the three pre-exec identity checks
/// (`enforceQuota` / `checkApiKeyModelAccess` / `applyModelMapping`), which Go
/// runs back-to-back as inbound middlewares. `Quota` is split out because the
/// S06 pure decision owns it. `Pipeline` covers the attempt loop (transform +
/// execute + retry + transform-back); `Persist` covers the
/// `persistRequestExecution` + usage-log write; `Emit` is the final client
/// response (stream or body).
///
/// Returns the same slice as [`OrchestratorStage::ALL`]; exposing it as a
/// function (rather than only the const) makes the Process contract explicit
/// and gives tests + diagnostics one canonical entry point.
pub fn stage_sequence() -> &'static [OrchestratorStage] {
    &OrchestratorStage::ALL
}

/// Pure description of one stage of the `Process` main chain. Captures what the
/// stage reads (inputs) and what it produces (outputs), mirroring the Go
/// `Process` data flow. No IO — this is a contract surface the wiring layer and
/// tests consult.
///
/// The `inputs` / `outputs` lists are short, lowercase string tags (matching
/// the field / artifact names from the Go source) so they can be diffed
/// against the Go data flow without re-reading the orchestrator code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePlan {
    /// The stage this plan describes.
    pub stage: OrchestratorStage,
    /// Short tag names of the artifacts the stage reads (e.g. `"api_key"`,
    /// `"system_settings"`, `"candidates"`). Order is the Go read order.
    pub inputs: &'static [&'static str],
    /// Short tag names of the artifacts the stage produces (e.g. `"quota_ok"`,
    /// `"ordered_candidates"`, `"http_response"`). Order is the Go write order.
    pub outputs: &'static [&'static str],
}

impl StagePlan {
    /// All seven stage plans, in [`stage_sequence`] order. The slice is the
    /// canonical Process contract — each entry's `inputs` mirror what the Go
    /// middleware chain reads at that point, and `outputs` mirror what it
    /// hands to the next stage.
    pub const fn table() -> &'static [StagePlan] {
        &[
            StagePlan {
                stage: OrchestratorStage::Auth,
                inputs: &["api_key", "profile", "system_settings"],
                outputs: &["api_key_resolved", "model_access_ok"],
            },
            StagePlan {
                stage: OrchestratorStage::Quota,
                inputs: &["api_key", "profile_quota", "quota_check_result"],
                outputs: &["quota_ok"],
            },
            StagePlan {
                stage: OrchestratorStage::Select,
                inputs: &["llm_request", "channels", "provider_quota"],
                outputs: &["candidates"],
            },
            StagePlan {
                stage: OrchestratorStage::LoadBalance,
                inputs: &["candidates", "retry_policy", "sticky_key"],
                outputs: &["ordered_candidates"],
            },
            StagePlan {
                stage: OrchestratorStage::Pipeline,
                // Go: inbound transform → outbound transform → execute → retry
                inputs: &["ordered_candidates", "http_request", "inbound", "outbound"],
                outputs: &["provider_response", "attempts"],
            },
            StagePlan {
                stage: OrchestratorStage::Persist,
                inputs: &["request", "request_execution", "usage", "attempts"],
                outputs: &["request_status", "usage_log"],
            },
            StagePlan {
                stage: OrchestratorStage::Emit,
                inputs: &["provider_response"],
                outputs: &["client_response"],
            },
        ]
    }

    /// Look up the plan for a given [`OrchestratorStage`]. Returns `None` for
    /// stages outside the main chain (there are none today, but the helper is
    /// total for forwards safety).
    pub fn for_stage(stage: OrchestratorStage) -> Option<&'static StagePlan> {
        Self::table().iter().find(|plan| plan.stage == stage)
    }
}

// ===========================================================================
// RUST-P9-006 S30 — system-bypass locality
// ===========================================================================
//
// Go usage of `authz.WithSystemBypass` / `authz.RunWithSystemBypass`:
//   * `orchestrator.go:164` applies `WithSystemBypass(ctx, "process-chat-
//     completion")` blanket for the whole `Process` flow. The bypass exists so
//     the middlewares can read internal data (system settings, channels, api
//     keys, prompts, prompt-protection rules, quota, request/usage log
//     writers) without per-call scopes.
//   * Most other Go call sites use the *scoped* `RunWithSystemBypass(fn)`
//     helper to run a single internal-data read under bypass — NOT a blanket
//     bypass (see `biz/auth.go:101` "auth-get-secret-key", `biz/quota.go:63`
//     "quota-request-count", `biz/prompt.go:41` "prompt-initialize", etc.).
//
// S30 encodes that distinction: which stages of the main chain need the system
// bypass (because they read internal data), and which do NOT (because they
// only touch the inbound request / the upstream provider, which is user-
// scoped). The pure [`bypass_scope`] decision below is the contract surface
// the wiring layer consults when it builds the bypass context per stage; it
// keeps the bypass **scoped to internal-data reads** rather than blanket-
// applied to every stage (mirroring Go's `RunWithSystemBypass` discipline).
//
// `[Pascal-the-3rd ?]`: Go's `Process` applies the bypass blanket-style for
// historical reasons (the middlewares each do their own internal reads). The
// Rust wiring layer is expected to honor this contract by wrapping only the
// internal-data reads in bypass scopes — i.e. producing the same observable
// behavior as Go without granting a blanket bypass to stages that never read
// internal data. This is the locality TODO S30 asks for.

/// Where the system bypass applies for a given main-chain stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassScope {
    /// The stage reads internal data (system settings, channels, api keys,
    /// prompts, prompt-protection rules, quota, request/usage logs) and so must
    /// run under the system bypass. Mirrors Go's `WithSystemBypass` /
    /// `RunWithSystemBypass(reason)` for that stage's reads.
    Internal,
    /// The stage only touches the inbound request or the upstream provider and
    /// does NOT read internal data — no bypass is granted. Keeps the bypass
    /// scoped rather than blanket (the S30 locality requirement).
    None,
}

impl BypassScope {
    /// `true` when this scope grants the system bypass.
    pub const fn is_internal(self) -> bool {
        matches!(self, Self::Internal)
    }
}

/// S30 — Pure decision: does this main-chain stage need the system bypass?
///
/// The bypass is granted ONLY for stages that read internal data:
/// * [`OrchestratorStage::Auth`] — reads the api-key profile + system model
///   settings (Go `applyAutoReasoningEffort` / `checkApiKeyModelAccess` /
///   `applyModelMapping` all consult `processor.SystemService`).
/// * [`OrchestratorStage::Quota`] — reads the api-key profile quota +
///   `quotaService.CheckAPIKeyQuota` (internal read).
/// * [`OrchestratorStage::Select`] — reads enabled channels + provider quota
///   (internal read; Go `selectCandidates` queries `ChannelService`).
/// * [`OrchestratorStage::Persist`] — writes the request / request-execution /
///   usage log (internal write; Go `persistRequest` /
///   `persistRequestExecution` / `CreateUsageLogFromRequest`).
///
/// Stages that only touch the inbound request or the upstream provider do NOT
/// get the bypass:
/// * [`OrchestratorStage::LoadBalance`] — pure scoring over already-resolved
///   candidates (no internal read).
/// * [`OrchestratorStage::Pipeline`] — drives the upstream provider call (the
///   bypass must NOT grant upstream-side privileges; provider calls are user-
///   scoped).
/// * [`OrchestratorStage::Emit`] — pure response shaping (no internal read).
///
/// `[Pascal-the-3rd ?]`: the Go `Process` blanket-applies the bypass, so this
/// pure decision is a *refinement* of the Go contract (the S30 locality TODO).
/// The observable behavior is preserved: the stages marked [`BypassScope::None`]
/// never read internal data in Go, so granting-or-not granting the bypass to
/// them is observationally equivalent. The refinement future-proofs against a
/// later stage accidentally reading internal data without an explicit scope.
pub fn bypass_scope(stage: OrchestratorStage) -> BypassScope {
    match stage {
        OrchestratorStage::Auth
        | OrchestratorStage::Quota
        | OrchestratorStage::Select
        | OrchestratorStage::Persist => BypassScope::Internal,
        OrchestratorStage::LoadBalance | OrchestratorStage::Pipeline | OrchestratorStage::Emit => {
            BypassScope::None
        }
    }
}

// ===========================================================================
// RUST-P9-006 S31/S32 — retry-policy derivation
// ===========================================================================
//
// Go source (`orchestrator.go:169` + `retry.go:93`):
//
//   retryPolicy := processor.SystemService.RetryPolicyOrDefault(ctx)        // S32
//   strategy    := deriveLoadBalancerStrategy(retryPolicy, apiKey)          // S31
//
//   // only apply retry options when enabled:
//   if retryPolicy.Enabled {
//       pipelineOpts = append(pipelineOpts, pipeline.WithRetry(
//           retryPolicy.MaxChannelRetries,
//           retryPolicy.MaxSingleChannelRetries,
//           time.Duration(retryPolicy.RetryDelayMs)*time.Millisecond))
//       if retryPolicy.EmptyResponseDetection {
//           pipelineOpts = append(pipelineOpts, pipeline.WithEmptyResponseDetection())
//       }
//       pipelineOpts = append(pipelineOpts, pipeline.WithResponseTimeouts(
//           time.Duration(retryPolicy.StreamFirstEventTimeoutSeconds)*time.Second,
//           time.Duration(retryPolicy.NonStreamResponseTimeoutSeconds)*time.Second))
//   }
//
// Go `deriveLoadBalancerStrategy` (`retry.go:93`):
//
//   func deriveLoadBalancerStrategy(retryPolicy *biz.RetryPolicy, apiKey *ent.APIKey) string {
//       strategy := retryPolicy.LoadBalancerStrategy
//       if apiKey == nil { return strategy }
//       activeProfile := apiKey.GetActiveProfile()
//       if activeProfile == nil { return strategy }
//       if activeProfile.LoadBalanceStrategy == nil ||
//          *activeProfile.LoadBalanceStrategy == "" ||
//          *activeProfile.LoadBalanceStrategy == "system_default" {
//           return strategy
//       }
//       return *activeProfile.LoadBalanceStrategy
//   }
//
// The [`crate::load_balancer::RetryPolicy`] port covers ONLY the LB-facing
// fields (`enabled`, retry counts, delay, strategy). The Process-level Go
// `biz.RetryPolicy` has additional fields the orchestrator wiring feeds into
// the pipeline options (`EmptyResponseDetection`,
// `StreamFirstEventTimeoutSeconds`, `NonStreamResponseTimeoutSeconds`) — those
// are NOT part of the LB struct. S31/S32 here ports the **Process-level**
// retry policy + its derivation (system default + per-API-key profile
// override), surfaced as a pure decision so the wiring layer can hand the
// resulting [`ProcessRetryPolicy`] to the pipeline option builders without
// re-reading the Go source.
//
// Parity details:
//   * The LB-facing fields are re-derived from the [`ProcessRetryPolicy`] via
//     [`ProcessRetryPolicy::to_lb_policy`], so there is one source of truth for
//     the derivation (the [`derive_retry_policy`] entry point).
//   * The default mirrors Go `defaultRetryPolicy` (`system_default.go`):
//     enabled, 3 / 2 retries, 1000ms delay, adaptive, empty-response detection
//     off, both timeouts 0 (disabled), upstream-error-policy passthrough.
//   * The API-key profile override only overrides the **load-balance
//     strategy** (Go `deriveLoadBalancerStrategy` reads ONLY the
//     `LoadBalanceStrategy` field of the active profile — not retry counts or
//     timeouts). The derivation below mirrors that exactly: a non-empty /
//     non-`"system_default"` profile strategy replaces the system strategy;
//     every other field stays at the system value. (Other Go code paths may
//     override retry counts per channel; those are not part of the S31
//     derivation contract.)
//   * The `weighted` strategy is normalized to `failover` (Go
//     `normalizeRetryPolicy`); that normalization lives in
//     [`crate::load_balancer::LoadBalancerStrategy::parse`], which the
//     derivation delegates to.

/// Go `biz.UpstreamErrorPolicy.Mode` values (passthrough / hidden / custom).
/// Mirrors `biz.UpstreamErrorModePassthrough` / `Hidden` / `Custom`.
pub const UPSTREAM_ERROR_MODE_PASSTHROUGH: &str = "passthrough";
/// Go `biz.UpstreamErrorModeHidden`.
pub const UPSTREAM_ERROR_MODE_HIDDEN: &str = "hidden";
/// Go `biz.UpstreamErrorModeCustom`.
pub const UPSTREAM_ERROR_MODE_CUSTOM: &str = "custom";

/// The "system default" sentinel value the API-key profile uses to opt out of
/// overriding the load-balance strategy (Go `deriveLoadBalancerStrategy`).
pub const SYSTEM_DEFAULT_SENTINEL: &str = "system_default";

/// The Process-level retry policy. Mirrors Go `biz.RetryPolicy` (the full
/// struct, not just the LB-facing subset). The orchestrator wiring builds the
/// pipeline options from this struct; the LB-facing subset is derived via
/// [`ProcessRetryPolicy::to_lb_policy`].
///
/// Field names match the Go json tags (snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessRetryPolicy {
    /// Go `RetryPolicy.Enabled`.
    pub enabled: bool,
    /// Go `RetryPolicy.MaxChannelRetries`.
    pub max_channel_retries: i64,
    /// Go `RetryPolicy.MaxSingleChannelRetries`.
    pub max_single_channel_retries: i64,
    /// Go `RetryPolicy.RetryDelayMs`.
    pub retry_delay_ms: i64,
    /// Go `RetryPolicy.StreamFirstEventTimeoutSeconds` (0 disables; clamped to
    /// 0..=600 by Go `normalizeRetryPolicy`).
    pub stream_first_event_timeout_seconds: i64,
    /// Go `RetryPolicy.NonStreamResponseTimeoutSeconds` (0 disables; clamped to
    /// 0..=600).
    pub non_stream_response_timeout_seconds: i64,
    /// Go `RetryPolicy.EmptyResponseDetection`.
    pub empty_response_detection: bool,
    /// Go `RetryPolicy.LoadBalancerStrategy` (post-API-key-override — see
    /// [`derive_retry_policy`]). Stored as the parsed enum so the wiring does
    /// not need to re-parse.
    pub load_balancer_strategy: crate::load_balancer::LoadBalancerStrategy,
    /// Go `RetryPolicy.UpstreamErrorPolicy.Mode`. One of
    /// [`UPSTREAM_ERROR_MODE_PASSTHROUGH`] / [`UPSTREAM_ERROR_MODE_HIDDEN`] /
    /// [`UPSTREAM_ERROR_MODE_CUSTOM`].
    pub upstream_error_mode: &'static str,
}

impl Default for ProcessRetryPolicy {
    /// Mirrors Go `defaultRetryPolicy` (`biz/system_default.go`): enabled, 3 / 2
    /// retries, 1000ms delay, adaptive, empty-response detection off, both
    /// timeouts 0 (disabled), upstream-error-policy passthrough.
    fn default() -> Self {
        Self {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 1000,
            stream_first_event_timeout_seconds: 0,
            non_stream_response_timeout_seconds: 0,
            empty_response_detection: false,
            load_balancer_strategy: crate::load_balancer::LoadBalancerStrategy::Adaptive,
            upstream_error_mode: UPSTREAM_ERROR_MODE_PASSTHROUGH,
        }
    }
}

impl ProcessRetryPolicy {
    /// Derive the LB-facing retry-policy subset (the existing
    /// [`crate::load_balancer::RetryPolicy`]). Used by the wiring layer when it
    /// hands the policy to `LoadBalancer::sort` / `select_channels`.
    ///
    /// Mirrors how Go threads the same `biz.RetryPolicy` into both the pipeline
    /// options (Process-level fields) and the `LoadBalancer` (LB-facing
    /// fields): one struct, two consumers.
    pub fn to_lb_policy(self) -> LbRetryPolicy {
        LbRetryPolicy {
            enabled: self.enabled,
            max_channel_retries: u32::try_from(self.max_channel_retries.max(0))
                .unwrap_or(0)
                .min(u32::MAX / 4),
            max_single_channel_retries: u32::try_from(self.max_single_channel_retries.max(0))
                .unwrap_or(0)
                .min(u32::MAX / 4),
            retry_delay_ms: u64::try_from(self.retry_delay_ms.max(0))
                .unwrap_or(0)
                .min(u64::MAX / 4),
            strategy: self.load_balancer_strategy,
        }
    }

    /// Whether the orchestrator wiring should attach
    /// `pipeline.WithEmptyResponseDetection()`. Mirrors Go's
    /// `if retryPolicy.EmptyResponseDetection { ... }` gate (Go only attaches
    /// it when both retry is enabled AND the flag is set; the wiring should
    /// additionally check [`Self::enabled`]).
    pub const fn attach_empty_response_detection(self) -> bool {
        self.enabled && self.empty_response_detection
    }

    /// Whether the orchestrator wiring should attach response timeouts (Go
    /// attaches them inside the `if retryPolicy.Enabled { ... }` block). Returns
    /// `true` when retry is enabled AND at least one timeout is non-zero (Go
    /// attaches them unconditionally inside the enabled block, but a 0/0 value
    /// is equivalent to "disabled" — surfacing it as a predicate lets the
    /// wiring skip the no-op call).
    pub const fn attach_response_timeouts(self) -> bool {
        self.enabled
            && (self.stream_first_event_timeout_seconds > 0
                || self.non_stream_response_timeout_seconds > 0)
    }
}

/// The API-key profile's view of the retry-policy override surface. The wiring
/// layer reads the active profile's `LoadBalanceStrategy` and hands it here.
///
/// Mirrors the fields of Go `objects.APIKeyProfile` that
/// `deriveLoadBalancerStrategy` reads. None of the other Go `RetryPolicy`
/// fields are overridable from the API-key profile (Go
/// `deriveLoadBalancerStrategy` reads ONLY `LoadBalanceStrategy`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiKeyProfileOverride {
    /// Go `activeProfile.LoadBalanceStrategy`. `None` mirrors Go's `nil`
    /// pointer; `Some("")` mirrors Go's empty string; `Some("system_default")`
    /// is the explicit opt-out sentinel. Any other non-empty value overrides
    /// the system strategy.
    pub load_balance_strategy: Option<String>,
}

impl ApiKeyProfileOverride {
    /// Build an override that does NOT override anything (mirrors Go's nil
    /// api-key / nil active-profile fast paths).
    pub fn none() -> Self {
        Self {
            load_balance_strategy: None,
        }
    }

    /// `true` when the profile supplies a strategy that should override the
    /// system strategy. Mirrors Go's
    /// `activeProfile.LoadBalanceStrategy != nil &&
    ///  *activeProfile.LoadBalanceStrategy != "" &&
    ///  *activeProfile.LoadBalanceStrategy != "system_default"`.
    pub fn overrides_strategy(&self) -> bool {
        match &self.load_balance_strategy {
            Some(s) => !s.is_empty() && s != SYSTEM_DEFAULT_SENTINEL,
            None => false,
        }
    }
}

/// S31/S32 — Derive the Process-level retry policy from the system default +
/// the API-key profile override (Go
/// `processor.SystemService.RetryPolicyOrDefault(ctx)` +
/// `deriveLoadBalancerStrategy(retryPolicy, apiKey)`).
///
/// Inputs:
/// * `system` — the system-level retry policy (Go `RetryPolicyOrDefault`
///   return; already normalized via Go `normalizeRetryPolicy`).
/// * `profile_override` — the active API-key profile's override surface (Go
///   `apiKey.GetActiveProfile()`). [`ApiKeyProfileOverride::none`] mirrors a
///   nil api-key / nil active-profile.
///
/// Derivation:
/// 1. Start from `system` (every field at the system value).
/// 2. When `profile_override.overrides_strategy()` is true, the profile's
///    load-balance strategy replaces the system strategy (Go
///    `deriveLoadBalancerStrategy`'s terminal `return
///    *activeProfile.LoadBalanceStrategy`). The raw string is parsed via
///    [`crate::load_balancer::LoadBalancerStrategy::parse`], which applies
///    Go's `"weighted" → failover` normalization.
/// 3. Every other field stays at the system value (Go's
///    `deriveLoadBalancerStrategy` does NOT read any other profile field).
///
/// Returns the derived [`ProcessRetryPolicy`]; the wiring layer hands its
/// LB-facing subset ([`ProcessRetryPolicy::to_lb_policy`]) to the load
/// balancer and its pipeline-facing fields to the pipeline option builders.
pub fn derive_retry_policy(
    system: ProcessRetryPolicy,
    profile_override: &ApiKeyProfileOverride,
) -> ProcessRetryPolicy {
    let mut derived = system;

    if profile_override.overrides_strategy() {
        // unwrap_or(empty) is safe: overrides_strategy() guarantees the value
        // is Some, non-empty, and not the sentinel — but defensive programming
        // keeps the helper total without an unwrap.
        let raw = profile_override
            .load_balance_strategy
            .as_deref()
            .unwrap_or("");
        derived.load_balancer_strategy = crate::load_balancer::LoadBalancerStrategy::parse(raw);
    }

    derived
}

// ===========================================================================
// RUST-P9-006 S33 — detached-10s-context failure persistence
// ===========================================================================
//
// Go source (`conduit/internal/server/orchestrator/orchestrator.go` Process
// error branch, lines 299-328):
//
//		result, err := pipe.Process(ctx, request)
//		if err != nil {
//			persistCtx, cancel := xcontext.DetachWithTimeout(ctx, time.Second*10)
//			defer cancel()
//
//			// Update the last request execution status based on error if it exists
//			if requestExec := outbound.GetRequestExecution(); requestExec != nil {
//				if updateErr := processor.RequestService.UpdateRequestExecutionStatusFromError(
//					persistCtx,
//					requestExec.ID,
//					err,
//				); updateErr != nil {
//					log.Warn(persistCtx, "Failed to update request execution status from error", log.Cause(updateErr))
//				}
//			}
//
//			// Update the main request status based on error
//			if request := outbound.GetRequest(); request != nil {
//				if updateErr := processor.RequestService.UpdateRequestStatusFromError(
//					persistCtx,
//					request.ID,
//					err,
//				); updateErr != nil {
//					log.Warn(persistCtx, "Failed to update request status from error", log.Cause(updateErr))
//				}
//			}
//
//			return ChatCompletionResult{}, err
//		}
//
// And (`request_execution.go` OnOutboundRawError, lines 216-229):
//
//		// Use context without cancellation to ensure persistence even if client canceled
//		persistCtx, cancel := xcontext.DetachWithTimeout(ctx, 10*time.Second)
//		defer cancel()
//
//		updateErr := state.RequestService.UpdateRequestExecutionFailed(
//			persistCtx,
//			state.RequestExec.ID,
//			ExtractErrorMessage(err),
//			ExtractErrorInfo(err),
//		)
//
// The load-bearing semantic is the DETACHED context: by the time failure
// persistence runs, the request `ctx` is already canceled (that's why the
// pipeline returned an error — typically `context.Canceled` on client
// disconnect, or a hard upstream failure that bubbled up while the request was
// still being torn down). `xcontext.DetachWithTimeout(ctx, 10*time.Second)`
// produces a fresh context that INHERITS the values from `ctx` but IGNORES its
// cancellation/deadline — replacing the parent's Done channel with a brand-new
// 10-second timer. So:
//
//   * persistence MUST still run even if the original request was canceled;
//   * the persistence call gets a hard 10-second ceiling (Go's `time.Second*10`);
//   * the only inputs to the persistence decision are (a) the final error and
//     (b) the request id the wiring layer has on hand.
//
// S33 surfaces that contract as a pure [`FailurePersistencePlan`]: the wiring
// layer calls [`failure_persistence_plan`] with the bubbled-up error + the
// request/execution ids it has resolved, then walks the plan against the
// (real, detached) `RequestService`. The plan itself does NO IO — it is the
// pure decision the Go error branch encodes, factored out so the failure path
// is unit-testable without spinning up a DB.
//
// Go parity details:
//   * `detached_timeout_ms` is exactly `10_000` (Go `time.Second*10` -> 10_000
//     ms). Exposed in milliseconds so the Rust wiring can hand it to a
//     `tokio::time::timeout` without re-deriving the constant.
//   * The plan always carries a `RequestStatus::Failed` terminal status. Go's
//     `UpdateRequestStatusFromError` maps the error to a status internally
//     (canceled -> request cancelled, otherwise failed); S33 exposes the
//     *orchestrator-side* terminal status as `Failed` (the request is leaving
//     the `Running` state as a terminal failure, never as `Succeeded`), while
//     the precise sub-status (cancelled vs failed) is owned by the recorder's
//     error->status mapper. See the `[Confucius-the-3rd ?]` note on
//     [`FailurePersistencePlan::final_request_status`].
//   * The error message is preserved verbatim so the recorder can store it on
//     the request/execution row (Go `ExtractErrorMessage`).

/// Detached-context timeout the Go orchestrator hands to failure persistence,
/// in milliseconds. Mirrors `time.Second*10` at
/// `conduit/internal/server/orchestrator/orchestrator.go:301` and
/// `request_execution.go:132` / `:217`.
pub const FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS: u64 = 10_000;

/// The terminal request status the Go error branch writes via
/// `UpdateRequestStatusFromError`. The Go recorder may internally distinguish
/// `request.StatusCanceled` from `request.StatusFailed` based on
/// `errors.Is(err, context.Canceled)`, but the orchestrator-side contract is
/// always "leave Running, mark as a terminal failure". This constant captures
/// the orchestrator-side view; the cancelled-vs-failed sub-status is the
/// recorder's responsibility.
///
/// `[Confucius-the-3rd ?]`: Go `UpdateRequestStatusFromError` is defined in
/// `internal/server/biz/request_service.go` and was not snapshotted for this
/// task. The plan exposes `Failed` as the safe default; if the recorder later
/// needs the canceled signal, [`FailurePersistencePlan::error_message`] can be
/// re-inspected for the `context canceled` marker Go uses.
pub const FAILURE_PERSISTENCE_TERMINAL_STATUS: RequestStatus = RequestStatus::Failed;

/// Pure description of the failure-persistence step the Go `Process` error
/// branch performs (RUST-P9-006 S33). No IO — captures (a) the terminal
/// status to write, (b) the error message to record, (c) the detached-context
/// timeout the wiring layer must apply around the recorder call, and (d) the
/// request / execution ids the recorder should update.
///
/// Field names mirror the Go data flow:
///   * `final_request_status` <- Go `UpdateRequestStatusFromError` target;
///   * `error_message`        <- Go `ExtractErrorMessage(err)` /
///                                 `err.Error()` for non-HTTP errors;
///   * `detached_timeout_ms`  <- Go `time.Second*10`;
///   * `request_id`           <- Go `outbound.GetRequest().ID`;
///   * `execution_id`         <- Go `outbound.GetRequestExecution().ID`
///                                 (optional — the execution may not yet
///                                 exist if the pipeline failed before
///                                 `persistRequestExecution` ran).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailurePersistencePlan {
    /// Terminal status the recorder writes on the request row. Always
    /// [`FAILURE_PERSISTENCE_TERMINAL_STATUS`] (Failed) — see the constant's
    /// doc for the cancelled-vs-failed rationale.
    pub final_request_status: RequestStatus,
    /// Error message extracted from the bubbled-up pipeline error, suitable
    /// for persisting on the request/execution row. Mirrors Go
    /// `ExtractErrorMessage(err)` (HTTP error body's `error.message` /
    /// `errors.0.message` / `errors.message`, falling back to `err.Error()`).
    pub error_message: String,
    /// Detached-context timeout in milliseconds. The wiring layer must apply
    /// `tokio::time::timeout(Duration::from_millis(detached_timeout_ms), ...)`
    /// around the recorder call so persistence survives the request-context
    /// cancellation.
    pub detached_timeout_ms: u64,
    /// Request id to mark failed (Go `outbound.GetRequest().ID`). `None` when
    /// the inbound `persistRequest` middleware never created the request row
    /// (Go guards with `if request := outbound.GetRequest(); request != nil`).
    pub request_id: Option<String>,
    /// Execution id to mark failed (Go `outbound.GetRequestExecution().ID`).
    /// `None` when the pipeline failed before `persistRequestExecution`
    /// produced an execution row (Go guards with
    /// `if requestExec := outbound.GetRequestExecution(); requestExec != nil`).
    pub execution_id: Option<String>,
}

impl FailurePersistencePlan {
    /// Whether this plan asks the wiring layer to update the request row at
    /// all. Mirrors Go's `if request := outbound.GetRequest(); request != nil`
    /// short-circuit.
    pub fn persists_request(&self) -> bool {
        self.request_id.is_some()
    }

    /// Whether this plan asks the wiring layer to update the execution row.
    /// Mirrors Go's `if requestExec := outbound.GetRequestExecution();
    /// requestExec != nil` short-circuit.
    pub fn persists_execution(&self) -> bool {
        self.execution_id.is_some()
    }
}

/// S33 — Build the failure-persistence plan for the pipeline-error branch of
/// Go `ChatCompletionOrchestrator.Process` (orchestrator.go:299-328).
///
/// Pure: produces a [`FailurePersistencePlan`] carrying the terminal Failed
/// status, the error message to record, the detached-context timeout (10_000
/// ms), and the request/execution ids the wiring has resolved. The wiring
/// layer then walks the plan against the recorder under a
/// `tokio::time::timeout` matching [`FailurePersistencePlan::detached_timeout_ms`].
///
/// `final_error` is the bubbled-up pipeline error (Go `err`). Its display
/// string becomes [`FailurePersistencePlan::error_message`] — the Go error
/// branch passes the raw `err` to `UpdateRequestStatusFromError`, and the
/// `request_execution.go` `OnOutboundRawError` path feeds it through
/// `ExtractErrorMessage` first. We surface the raw `err.to_string()` here and
/// leave the HTTP-body extraction to the recorder (which owns the
/// `httpclient.Error` shape).
///
/// `request_id` / `execution_id` are `Option`, mirroring Go's nil-checks on
/// `outbound.GetRequest()` / `outbound.GetRequestExecution()`.
pub fn failure_persistence_plan(
    final_error: &ConduitError,
    request_id: Option<String>,
    execution_id: Option<String>,
) -> FailurePersistencePlan {
    FailurePersistencePlan {
        final_request_status: FAILURE_PERSISTENCE_TERMINAL_STATUS,
        error_message: final_error.to_string(),
        detached_timeout_ms: FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS,
        request_id,
        execution_id,
    }
}

// ===========================================================================
// RUST-P9-006 S34 — stream live-preview + final persistence
// ===========================================================================
//
// Go source — streaming response final persistence
// (`conduit/internal/server/orchestrator/outbound.go` `OutboundPersistentStream.Close`,
// lines 100-212):
//
//		func (ts *OutboundPersistentStream) Close() error {
//			if ts.closed { return nil }
//			ts.closed = true
//			ctx := ts.ctx
//			...
//			streamErr := ts.stream.Err()
//			ctxErr := ctx.Err()
//
//			// If we received the [DONE] event, treat the stream as successfully
//			// completed even if there's a context cancellation error.
//			if ts.state.StreamCompleted {
//				// Stream completed successfully - perform final persistence
//				ts.persistResponseChunks(ctx)
//				return ts.stream.Close()
//			}
//
//			// If there's an explicit stream error (not just context
//			// cancellation), treat as failure ...
//			if streamErr != nil && !errors.Is(streamErr, context.Canceled) && !errors.Is(streamErr, context.DeadlineExceeded) {
//				persistCtx, cancel := xcontext.DetachWithTimeout(ctx, 10*time.Second)
//				defer cancel()
//				if ts.requestExec != nil {
//					... UpdateRequestExecutionStatusFromError(persistCtx, ts.requestExec.ID, streamErr) ...
//				}
//				return ts.stream.Close()
//			}
//
//			// ... aggregation attempt ...
//
//			// ended without a terminal event / complete aggregated response.
//			if (ctxErr != nil || streamErr != nil) && !ts.state.StreamCompleted {
//				persistCtx, cancel := xcontext.DetachWithTimeout(ctx, 10*time.Second)
//				defer cancel()
//				... UpdateRequestExecutionStatusFromError(persistCtx, ts.requestExec.ID, errToReport) ...
//				return ts.stream.Close()
//			}
//
//			if !ts.state.StreamCompleted {
//				... UpdateRequestExecutionStatusFromError(... errors.New("stream ended without terminal event or completed response")) ...
//				return ts.stream.Close()
//			}
//
//			// Stream completed successfully - perform final persistence
//			... persistResponseChunks / persistAggregatedResponse ...
//			return ts.stream.Close()
//		}
//
// Live-preview path (`live_streaming.go`): on every `Next()` the buffered
// stream forwards the raw event to the client unchanged (Current() returns
// the unmodified event) while Append()ing a (binary-summarized) copy into the
// per-request / per-execution live-preview buffer. The live-preview write is
// *transparent* to the client: nothing is withheld, nothing is rewritten.
//
// S34 surfaces the pure final-persistence decision the `Close` method encodes
// as [`StreamFinalPlan`]. Inputs are two booleans the wiring layer observes:
//
//   * `completed_normally`     — true when the terminal event arrived (Go
//                                 `ts.state.StreamCompleted`), or the aggregated
//                                 chunks form a complete response (`isCompletedAggregated`).
//   * `client_disconnected`    — true when the request context was canceled
//                                 (`ctx.Err() != nil`) or the stream surfaced a
//                                 `context.Canceled` / `context.DeadlineExceeded`.
//
// Mapping to the Go branches:
//
//   * completed_normally == true
//       -> Go: "Stream completed successfully - perform final persistence"
//          write_chunks = true, write_usage = true, final = Succeeded.
//   * completed_normally == false && client_disconnected == true
//       -> Go: "incomplete_stream_with_error" branch — persist a Cancelled
//          status, do NOT write chunks/usage (the response is incomplete and
//          must not be persisted as if it were a real completion).
//   * completed_normally == false && client_disconnected == false
//       -> Go: "incomplete_stream_without_terminal_event" branch — the stream
//          ended without [DONE] and without a client cancel; persist a Failed
//          status with the "stream ended without terminal event or completed
//          response" sentinel, do NOT write chunks/usage.
//
// `[Confucius-the-3rd ?]`: Go distinguishes the *explicit-stream-error* path
// (non-cancel upstream error) from the no-event-no-cancel path, but both end
// in `UpdateRequestExecutionStatusFromError`. S34 collapses them into the
// `completed_normally=false, client_disconnected=false` arm with
// [`STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE`] as the sentinel — matching Go's
// `errors.New("stream ended without terminal event or completed response")`
// from the no-event branch. The explicit-error branch carries the upstream
// error message instead; the wiring layer is expected to substitute that into
// [`StreamFinalPlan::error_message`] when it has a richer stream error on
// hand. The collapsed terminal status (`Failed`) is the same in both Go
// branches.

/// Detached-context timeout the Go stream-Close path hands to final
/// persistence, in milliseconds. Mirrors the `xcontext.DetachWithTimeout(ctx,
/// 10*time.Second)` calls at `outbound.go:130`, `:162`, `:184`, and `:246`.
pub const STREAM_FINAL_DETACHED_TIMEOUT_MS: u64 = 10_000;

/// Sentinel error message the Go stream-Close path records when the stream
/// ended without a terminal event and without a client disconnect. Verbatim
/// from `conduit/internal/server/orchestrator/outbound.go:170` and `:187`:
///
///   errors.New("stream ended without terminal event or completed response")
pub const STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE: &str =
    "stream ended without terminal event or completed response";

/// Terminal request status Go writes on the stream-success path. The Go
/// streaming test (`orchestrator_streaming_test.go:189`) asserts
/// `request.StatusCompleted == requests[0].Status` after a normal stream
/// close; the Rust enum equivalent is [`RequestStatus::Succeeded`].
pub const STREAM_FINAL_COMPLETED_STATUS: RequestStatus = RequestStatus::Succeeded;

/// Terminal request status Go writes when a stream ends because the client
/// went away. The Go stream-Close path routes this through
/// `UpdateRequestExecutionStatusFromError(persistCtx, id, ctxErr)` where
/// `ctxErr` is `context.Canceled`; the recorder maps canceled -> the
/// request-cancelled status. S34 exposes [`RequestStatus::Cancelled`] as the
/// orchestrator-side terminal status for this arm.
pub const STREAM_FINAL_CANCELED_STATUS: RequestStatus = RequestStatus::Cancelled;

/// Terminal request status Go writes when a stream ends abnormally without a
/// client disconnect (upstream error or no-terminal-event). Same mapping as
/// S33's failure path.
pub const STREAM_FINAL_FAILED_STATUS: RequestStatus = RequestStatus::Failed;

/// Pure description of the stream-final-persistence step the Go
/// `OutboundPersistentStream.Close` method performs (RUST-P9-006 S34). No IO
/// — captures (a) the terminal request status to write, (b) whether the
/// aggregated chunks should be persisted, (c) whether the usage log should be
/// written, (d) the error message to record on a failure path, and (e) the
/// detached-context timeout the wiring layer must apply around the recorder
/// call.
///
/// Field semantics mirror the Go `Close` branches:
///   * `final_status`    <- Succeeded on normal completion; Cancelled on
///                          client disconnect; Failed on upstream error /
///                          no-terminal-event;
///   * `write_chunks`    <- Go `persistResponseChunks` / `persistAggregatedResponse`
///                          (only on the Succeeded branch);
///   * `write_usage`     <- Go `persistAggregatedResponse`'s usage-log write
///                          (only on the Succeeded branch, when the aggregated
///                          `meta.Usage != nil`);
///   * `error_message`   <- the sentinel or upstream error string recorded on
///                          the non-Succeeded branches (`None` on Succeeded);
///   * `detached_timeout_ms` <- the 10s detached ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFinalPlan {
    /// Terminal request status the recorder writes for the request row.
    pub final_status: RequestStatus,
    /// Whether the recorder should persist the aggregated response chunks
    /// and update the execution row to Succeeded. True only on the normal-
    /// completion branch.
    pub write_chunks: bool,
    /// Whether the recorder should create a usage-log row from the aggregated
    /// response. True only on the normal-completion branch; the wiring layer
    /// additionally gates on `meta.Usage.is_some()` (Go `if usage :=
    /// meta.Usage; usage != nil`).
    pub write_usage: bool,
    /// Error message the recorder should persist on the execution row when
    /// `final_status != Succeeded`. Mirrors Go's
    /// `UpdateRequestExecutionStatusFromError(persistCtx, id, errToReport)`.
    /// `None` on the Succeeded branch.
    pub error_message: Option<String>,
    /// Detached-context timeout in milliseconds (always
    /// [`STREAM_FINAL_DETACHED_TIMEOUT_MS`]).
    pub detached_timeout_ms: u64,
}

impl StreamFinalPlan {
    /// Whether this plan represents a normal stream completion (the Succeeded
    /// branch). Convenience accessor mirroring the Go
    /// `if ts.state.StreamCompleted` fast path.
    pub fn is_completed(&self) -> bool {
        self.final_status == STREAM_FINAL_COMPLETED_STATUS
    }

    /// Whether this plan represents a client-disconnect cancellation. Mirrors
    /// the Go `(ctxErr != nil || streamErr != nil) && !StreamCompleted` arm
    /// where the underlying error is `context.Canceled`.
    pub fn is_canceled(&self) -> bool {
        self.final_status == STREAM_FINAL_CANCELED_STATUS
    }
}

/// S34 — Build the stream-final-persistence plan for the streaming-response
/// close path of Go `OutboundPersistentStream.Close` (outbound.go:100-212).
///
/// Pure: takes two booleans the wiring layer observes on stream close and
/// returns the [`StreamFinalPlan`] the recorder should execute under a
/// detached 10s context.
///
/// Arguments:
/// * `completed_normally`  — true when the terminal event arrived (Go
///   `ts.state.StreamCompleted`) OR the aggregated chunks form a complete
///   response (`isCompletedAggregated(meta)`). The wiring layer must already
///   have OR-ed these two signals before calling this; the Go `Close` body
///   folds the aggregation result into `StreamCompleted` at line 153.
/// * `client_disconnected` — true when the request context was canceled
///   (`ctx.Err() != nil`) or the stream surfaced a `context.Canceled` /
///   `context.DeadlineExceeded` error. Mirrors Go's
///   `errors.Is(streamErr, context.Canceled)` /
///   `errors.Is(streamErr, context.DeadlineExceeded)` checks plus the
///   `ctxErr != nil` short-circuit.
///
/// Branch table (mirrors Go `Close`):
///
/// | completed_normally | client_disconnected | final_status | write_chunks | write_usage |
/// |--------------------|---------------------|--------------|--------------|-------------|
/// | true               | *                   | Succeeded    | true         | true        |
/// | false              | true                | Cancelled    | false        | false       |
/// | false              | false               | Failed       | false        | false       |
///
/// Note the completed row ignores `client_disconnected`: Go's
/// `if ts.state.StreamCompleted` branch fires first and treats the stream as
/// successfully completed *even if there's a context cancellation error*
/// (explicit comment at outbound.go:114-117 — "This handles the case where
/// the client disconnects immediately after receiving the last chunk").
pub fn stream_final_plan(completed_normally: bool, client_disconnected: bool) -> StreamFinalPlan {
    if completed_normally {
        // Go: "Stream completed successfully - perform final persistence"
        // The Succeeded branch always writes chunks; usage is gated on
        // meta.Usage at the recorder, but the plan asks for it.
        return StreamFinalPlan {
            final_status: STREAM_FINAL_COMPLETED_STATUS,
            write_chunks: true,
            write_usage: true,
            error_message: None,
            detached_timeout_ms: STREAM_FINAL_DETACHED_TIMEOUT_MS,
        };
    }

    if client_disconnected {
        // Go: "incomplete_stream_with_error" branch — ctxErr/streamErr is a
        // cancellation. The actual error message recorded is the canceled
        // error's string; the wiring layer may overwrite `error_message` with
        // the richer `streamErr.to_string()` when available. Default to the
        // Go sentinel for context.Canceled.
        return StreamFinalPlan {
            final_status: STREAM_FINAL_CANCELED_STATUS,
            write_chunks: false,
            write_usage: false,
            error_message: Some("context canceled".to_string()),
            detached_timeout_ms: STREAM_FINAL_DETACHED_TIMEOUT_MS,
        };
    }

    // Go: "incomplete_stream_without_terminal_event" branch — neither a
    // terminal event nor a client cancel. Persist Failed with the Go sentinel.
    StreamFinalPlan {
        final_status: STREAM_FINAL_FAILED_STATUS,
        write_chunks: false,
        write_usage: false,
        error_message: Some(STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE.to_string()),
        detached_timeout_ms: STREAM_FINAL_DETACHED_TIMEOUT_MS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateStatus};
    use conduit_llm::{ApiFormat, ChatRequest, LlmRequestPayload, RequestType};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct AdmissionLifecycleRecorder {
        acquired: AtomicUsize,
        released: AtomicUsize,
    }

    struct CancellationLifecycleRecorder {
        acquired: AtomicUsize,
        released: AtomicUsize,
        block_reservation: bool,
    }

    impl CancellationLifecycleRecorder {
        fn new(block_reservation: bool) -> Self {
            Self {
                acquired: AtomicUsize::new(0),
                released: AtomicUsize::new(0),
                block_reservation,
            }
        }
    }

    #[async_trait]
    impl RequestRecorder for AdmissionLifecycleRecorder {
        fn acquire_api_key_slot(
            &self,
            ctx: &mut OrchestratorContext,
            api_key_id: i64,
            _limit: u32,
        ) -> Result<(), ConduitError> {
            self.acquired.fetch_add(1, Ordering::SeqCst);
            ctx.metadata.insert(
                "api_key_concurrency_slot".to_owned(),
                api_key_id.to_string(),
            );
            Ok(())
        }

        fn release_api_key_slot(&self, _ctx: &OrchestratorContext) {
            self.released.fetch_add(1, Ordering::SeqCst);
        }

        async fn reserve_request(
            &self,
            _ctx: &mut OrchestratorContext,
            _input: &BillingAdmissionInput,
        ) -> Result<(), ConduitError> {
            Err(ConduitError::quota_exhausted("wallet balance exhausted"))
        }

        async fn record_success(
            &self,
            _ctx: &OrchestratorContext,
            _request_id: &str,
            _project_id: &str,
            _attempt: &PipelineAttempt,
            _response: &HttpResponse,
        ) -> Result<(), ConduitError> {
            Ok(())
        }

        async fn record_failure(
            &self,
            _ctx: &OrchestratorContext,
            _request_id: &str,
            _project_id: &str,
            _error: &ConduitError,
        ) -> Result<(), ConduitError> {
            Ok(())
        }
    }

    #[async_trait]
    impl RequestRecorder for CancellationLifecycleRecorder {
        fn acquire_api_key_slot(
            &self,
            ctx: &mut OrchestratorContext,
            api_key_id: i64,
            _limit: u32,
        ) -> Result<(), ConduitError> {
            self.acquired.fetch_add(1, Ordering::SeqCst);
            ctx.metadata.insert(
                "api_key_concurrency_slot".to_owned(),
                api_key_id.to_string(),
            );
            Ok(())
        }

        fn release_api_key_slot(&self, _ctx: &OrchestratorContext) {
            self.released.fetch_add(1, Ordering::SeqCst);
        }

        async fn reserve_request(
            &self,
            _ctx: &mut OrchestratorContext,
            _input: &BillingAdmissionInput,
        ) -> Result<(), ConduitError> {
            if self.block_reservation {
                std::future::pending().await
            } else {
                Ok(())
            }
        }

        async fn record_success(
            &self,
            _ctx: &OrchestratorContext,
            _request_id: &str,
            _project_id: &str,
            _attempt: &PipelineAttempt,
            _response: &HttpResponse,
        ) -> Result<(), ConduitError> {
            Ok(())
        }

        async fn record_failure(
            &self,
            _ctx: &OrchestratorContext,
            _request_id: &str,
            _project_id: &str,
            _error: &ConduitError,
        ) -> Result<(), ConduitError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_wallet_reservation_releases_api_key_concurrency_slot() {
        let recorder = AdmissionLifecycleRecorder::default();
        let mut ctx = OrchestratorContext::new();
        let input = BillingAdmissionInput {
            request_key: "request-1".to_owned(),
            project_id: "project-1".to_owned(),
            api_key_id: Some("7".to_owned()),
            public_model: "gpt-5".to_owned(),
            estimated_input_tokens: 1,
            max_output_tokens: 1,
        };

        let error = match recorder.admit_request(&mut ctx, &input, Some(1)).await {
            Ok(_) => panic!("wallet admission must fail"),
            Err(error) => error,
        };

        assert_eq!(error.error_type(), "quota_exhausted");
        assert_eq!(recorder.acquired.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.released.load(Ordering::SeqCst), 1);
    }

    fn billing_admission(request_key: &str) -> BillingAdmissionInput {
        BillingAdmissionInput {
            request_key: request_key.to_owned(),
            project_id: "project-1".to_owned(),
            api_key_id: Some("7".to_owned()),
            public_model: "gpt-5".to_owned(),
            estimated_input_tokens: 1,
            max_output_tokens: 1,
        }
    }

    #[tokio::test]
    async fn timeout_while_reserving_releases_acquired_admission() {
        let recorder = CancellationLifecycleRecorder::new(true);
        let mut ctx = OrchestratorContext::new();

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            recorder.admit_request(&mut ctx, &billing_admission("request-reserve"), Some(1)),
        )
        .await;

        assert!(result.is_err(), "reservation future must be canceled");
        assert_eq!(recorder.acquired.load(Ordering::SeqCst), 1);
        assert_eq!(
            recorder.released.load(Ordering::SeqCst),
            1,
            "the borrowed admission guard must release on future Drop"
        );
        assert_eq!(
            ctx.metadata
                .get(BILLING_ADMISSION_REQUEST_KEY_METADATA)
                .map(String::as_str),
            Some("request-reserve")
        );
    }

    #[tokio::test]
    async fn timeout_after_admission_releases_request_guard() {
        let concrete = Arc::new(CancellationLifecycleRecorder::new(false));
        let recorder: Arc<dyn RequestRecorder> = concrete.clone();

        let admitted_then_stalled = async move {
            let mut ctx = OrchestratorContext::new();
            recorder
                .admit_request(&mut ctx, &billing_admission("request-pipeline"), Some(1))
                .await?;
            let _guard = RequestAdmissionGuard::new(
                Arc::clone(&recorder),
                &ctx,
                "test pipeline cancellation",
            );
            std::future::pending::<Result<(), ConduitError>>().await
        };

        let result =
            tokio::time::timeout(std::time::Duration::from_millis(10), admitted_then_stalled).await;

        assert!(result.is_err(), "request future must be canceled");
        assert_eq!(concrete.acquired.load(Ordering::SeqCst), 1);
        assert_eq!(
            concrete.released.load(Ordering::SeqCst),
            1,
            "the request admission guard must release on outer timeout"
        );
    }

    #[test]
    fn request_sticky_metadata_builds_fixed_channel_provider() {
        let mut ctx = OrchestratorContext::new();
        ctx.metadata.insert(
            STICKY_CHANNEL_ID_METADATA.to_string(),
            "channel-22".to_string(),
        );

        assert_eq!(
            request_sticky_provider(&ctx)
                .and_then(|provider| provider.sticky_channel(Some("trace-7"), None)),
            Some("channel-22".to_string())
        );
    }

    fn llm_request(stream: bool) -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("dummy-model".to_string()),
            stream,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    #[test]
    fn skeleton_records_fixed_stage_order() -> OrchestratorResult<()> {
        let orchestrator = SkeletonOrchestrator::new();
        let mut ctx = OrchestratorContext::new();

        let response = orchestrator.process(&mut ctx, llm_request(false).into())?;

        assert_eq!(ctx.stages, OrchestratorStage::ALL);
        assert_eq!(
            response,
            OrchestratorResponse::Http(HttpResponse {
                status: 202,
                metadata: BTreeMap::from([(
                    "orchestrator".to_string(),
                    json!("skeleton_no_provider_call"),
                )]),
                ..HttpResponse::default()
            })
        );
        Ok(())
    }

    #[test]
    fn skeleton_preserves_failed_stage() {
        let orchestrator = SkeletonOrchestrator::failing_at(OrchestratorStage::Pipeline);
        let mut ctx = OrchestratorContext::new();

        let err = orchestrator
            .process(&mut ctx, llm_request(false).into())
            .err();

        assert_eq!(
            ctx.stages,
            vec![
                OrchestratorStage::Auth,
                OrchestratorStage::Quota,
                OrchestratorStage::Select,
                OrchestratorStage::LoadBalance,
                OrchestratorStage::Pipeline,
            ]
        );
        assert_eq!(
            err.as_ref().map(|err| err.failed_stage),
            Some(OrchestratorStage::Pipeline)
        );
        assert_eq!(
            err.as_ref().map(|err| err.source.error_type()),
            Some("internal_error")
        );
    }

    #[test]
    fn streaming_request_returns_stream_placeholder() -> OrchestratorResult<()> {
        let orchestrator = SkeletonOrchestrator::new();
        let mut ctx = OrchestratorContext::new();

        let response = orchestrator.process(&mut ctx, llm_request(true).into())?;

        assert_eq!(
            response,
            OrchestratorResponse::Stream(OrchestratorStream::default())
        );
        Ok(())
    }

    #[test]
    fn attempt_records_generate_stable_one_based_execution_ids() {
        let candidate = Candidate::new("channel-a", "provider-a", "gpt-4o", CandidateStatus::Ready);

        let first = AttemptRecord::for_candidate("req-1", "project-a", 1, &candidate);
        let second = AttemptRecord::for_candidate("req-1", "project-a", 2, &candidate);

        assert_eq!(first.id, "req-1-attempt-1");
        assert_eq!(first.sequence, 1);
        assert_eq!(second.id, "req-1-attempt-2");
        assert_eq!(second.sequence, 2);
    }

    #[test]
    fn attempt_record_maps_success_and_failure_final_status() {
        let candidate = Candidate::new("channel-a", "provider-a", "gpt-4o", CandidateStatus::Ready);

        let success = AttemptRecord::for_candidate("req-1", "project-a", 1, &candidate)
            .succeeded()
            .to_execution_record();
        let failure = AttemptRecord::for_candidate("req-1", "project-a", 2, &candidate)
            .failed()
            .to_execution_record();

        assert_eq!(success.status, RequestStatus::Succeeded);
        assert_eq!(failure.status, RequestStatus::Failed);
    }

    #[test]
    fn attempt_record_preserves_channel_provider_and_model_on_execution() {
        let candidate = Candidate::new(
            "channel-b",
            "provider-b",
            "gpt-4o-mini",
            CandidateStatus::Ready,
        );

        let execution =
            AttemptRecord::for_candidate("req-2", "project-b", 3, &candidate).to_execution_record();

        assert_eq!(execution.id, "req-2-attempt-3");
        assert_eq!(execution.request_id, "req-2");
        assert_eq!(execution.project_id, "project-b");
        assert_eq!(execution.attempt, 3);
        assert_eq!(execution.provider.as_deref(), Some("provider-b"));
        assert_eq!(execution.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(execution.extra["channel_id"], "channel-b");
    }

    #[test]
    fn orchestrator_crate_does_not_directly_depend_on_axum() {
        let manifest = include_str!("../Cargo.toml");

        assert!(
            !manifest
                .lines()
                .any(|line| line.trim_start().starts_with("axum"))
        );
    }

    // =========================================================================
    // S05 — stream.EnsureUsage tests (mirror Go `usage_test.go` golden cases)
    // =========================================================================

    /// Read the chat payload for assertions; panics if it is not the chat variant
    /// (test-only).
    fn chat_ref(req: &LlmRequest) -> &ChatRequest {
        match &req.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => panic!("payload must be chat"),
        }
    }

    /// Mirrors Go `TestEnsureUsage_StreamEnabled`: stream on, no prior
    /// stream_options → object is created with `include_usage: true`.
    #[test]
    fn s05_ensure_usage_stream_enabled_creates_options() {
        let mut req = llm_request(true);
        // Preconditions: streaming, no stream_options.
        assert!(req.stream);
        assert!(chat_ref(&req).stream_options.is_none());

        let outcome = ensure_usage(&mut req);

        assert_eq!(
            outcome,
            EnsureUsageOutcome::ForcedIncludeUsage { created: true }
        );

        let opts = chat_ref(&req).stream_options.as_ref();
        assert!(opts.is_some(), "stream_options must be populated");
        assert_eq!(
            opts.and_then(|v| v.get(STREAM_OPTIONS_INCLUDE_USAGE_KEY)),
            Some(&json!(true))
        );
    }

    /// Mirrors Go `TestEnsureUsage_StreamEnabledWithExistingOptions`: stream on,
    /// stream_options present with `include_usage: false` → forced to `true`
    /// (the object is NOT recreated; other keys are preserved).
    #[test]
    fn s05_ensure_usage_stream_enabled_with_existing_options_forces_true() {
        let mut req = llm_request(true);
        // Start from an object that has extra keys + include_usage=false, exactly
        // like the Go golden case.
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            stream_options: Some(json!({
                "include_usage": false,
                "custom_marker": "keep-me",
            })),
            ..ChatRequest::default()
        });

        let outcome = ensure_usage(&mut req);

        assert_eq!(
            outcome,
            EnsureUsageOutcome::ForcedIncludeUsage { created: false }
        );

        let opts = chat_ref(&req)
            .stream_options
            .as_ref()
            .and_then(Value::as_object);
        assert!(opts.is_some(), "stream_options must stay an object");
        let map = match opts {
            Some(map) => map,
            None => return,
        };
        assert_eq!(
            map.get(STREAM_OPTIONS_INCLUDE_USAGE_KEY),
            Some(&json!(true))
        );
        // Other keys preserved.
        assert_eq!(map.get("custom_marker"), Some(&json!("keep-me")));
    }

    /// Mirrors Go `TestEnsureUsage_StreamDisabled`: stream off → middleware is
    /// a no-op (stream_options left as-is).
    #[test]
    fn s05_ensure_usage_stream_disabled_is_noop() {
        let mut req = llm_request(false);
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            stream_options: Some(json!({"include_usage": false})),
            ..ChatRequest::default()
        });

        let outcome = ensure_usage(&mut req);

        assert_eq!(outcome, EnsureUsageOutcome::NotStreaming);
        // Unchanged.
        assert_eq!(
            chat_ref(&req).stream_options,
            Some(json!({"include_usage": false}))
        );
    }

    /// Mirrors Go `TestEnsureUsage_StreamNil`: in Rust `stream` is a `bool`
    /// (never nil), so the closest analog is "stream explicitly false" — covered
    /// by `s05_ensure_usage_stream_disabled_is_noop`. We additionally cover the
    /// "stream false + no stream_options" combination to assert no allocation
    /// happens on the no-op path.
    #[test]
    fn s05_ensure_usage_stream_false_with_no_options_is_noop() {
        let mut req = llm_request(false);
        assert!(chat_ref(&req).stream_options.is_none());

        let outcome = ensure_usage(&mut req);

        assert_eq!(outcome, EnsureUsageOutcome::NotStreaming);
        assert!(
            chat_ref(&req).stream_options.is_none(),
            "non-streaming request must not allocate stream_options"
        );
    }

    /// Defensive: a non-object stream_options value is replaced with the
    /// canonical object (never panics). The Go struct round-trip would never
    /// produce this, but the Rust port's `Option<Value>` admits it.
    #[test]
    fn s05_ensure_usage_replaces_non_object_stream_options() {
        let mut req = llm_request(true);
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            stream_options: Some(json!("not-an-object")),
            ..ChatRequest::default()
        });

        let outcome = ensure_usage(&mut req);

        assert_eq!(
            outcome,
            EnsureUsageOutcome::ForcedIncludeUsage { created: true }
        );
        assert_eq!(
            chat_ref(&req).stream_options,
            Some(json!({STREAM_OPTIONS_INCLUDE_USAGE_KEY: true}))
        );
    }

    // =========================================================================
    // S06 — enforceQuota tests (mirror Go `quota_minute_test.go` intent)
    // =========================================================================

    /// Mirrors the success half of Go
    /// `TestChatCompletionOrchestrator_Process_MinuteQuotaExceeded`: when the
    /// quota service reports the request is allowed, the middleware admits it
    /// (no error).
    #[test]
    fn s06_enforce_quota_allowed_admits() {
        let result = QuotaCheckResultView::allowed();

        let outcome = enforce_quota(&result);

        assert!(outcome.is_ok(), "allowed result should admit: {outcome:?}");
    }

    /// Mirrors the denied half of Go
    /// `TestChatCompletionOrchestrator_Process_MinuteQuotaExceeded`: when the
    /// quota service denies, the middleware returns a quota error with the
    /// Go `llm.ResponseError` shape (code=`quota_exceeded`, type via
    /// `error_type()`=`quota_exceeded`, http_status=403, message=the service's
    /// denial message).
    #[test]
    fn s06_enforce_quota_denied_maps_to_quota_exhausted_with_go_shape() {
        let result = QuotaCheckResultView::denied("quota exceeded for the current minute");

        let err = match enforce_quota(&result) {
            Ok(()) => panic!("denied result should error"),
            Err(err) => err,
        };

        // Kind-level taxonomy routes through QuotaExhausted (Rust-side error
        // type), but the Go-parity code is overridden to "quota_exceeded".
        assert_eq!(err.error_type(), QUOTA_EXCEEDED_CODE);
        assert_eq!(err.http_status, QUOTA_EXCEEDED_HTTP_STATUS);
        // The service's denial message is preserved as both the diagnostic and
        // the public/safe message.
        assert_eq!(err.message, "quota exceeded for the current minute");
        assert_eq!(
            err.public_message(),
            "quota exceeded for the current minute"
        );
    }

    /// Defensive: when the service somehow denies with an empty message, the
    /// error still carries a non-empty safe message (mirrors Go's biz layer
    /// always populating Message, and avoids leaking an empty body to clients).
    #[test]
    fn s06_enforce_quota_denied_empty_message_falls_back() {
        let result = QuotaCheckResultView {
            allowed: false,
            message: String::new(),
            window: QuotaWindowView::default(),
        };

        let err = match enforce_quota(&result) {
            Ok(()) => panic!("denied result should error"),
            Err(err) => err,
        };

        assert!(!err.public_message().is_empty());
        assert_eq!(err.error_type(), QUOTA_EXCEEDED_CODE);
        assert_eq!(err.http_status, QUOTA_EXCEEDED_HTTP_STATUS);
    }

    /// The Go error shape uses `code = "quota_exceeded"` and
    /// `type = "quota_exceeded_error"`. The Rust port's `error_type()` resolves
    /// to the overridden code; this test pins the Go-parity constant so a future
    /// refactor that drops the override is caught.
    #[test]
    fn s06_quota_exceeded_constants_match_go_response_error() {
        assert_eq!(QUOTA_EXCEEDED_CODE, "quota_exceeded");
        assert_eq!(QUOTA_EXCEEDED_ERROR_TYPE, "quota_exceeded_error");
        assert_eq!(QUOTA_EXCEEDED_HTTP_STATUS, 403);
    }

    // [Faraday-the-26th] PENDING: Go `quota_minute_test.go` (123 lines, 1 test)
    // — `TestChatCompletionOrchestrator_Process_MinuteQuotaExceeded` is an
    // end-to-end DB-backed integration test in the original Go suite. It
    // verifies that after exhausting a 1-minute
    // API-key quota (1 request/minute), the SECOND Process call returns a 403
    // `quota_exceeded` error. The pure-logic decision (`enforce_quota` →
    // `QuotaExhausted` with Go error shape) is covered by the S06 tests above.
    // The PostgreSQL integration-level assertions (actual minute-window
    // counter advancing, real Process flow with channel selection + execution
    // + usage log) are blocked: the full orchestrator Process chain is not yet
    // wired in Rust.

    /// The window fields surface for diagnostics (Go logs `window_start` /
    /// `window_end`). Verify they ride along on the view without affecting the
    /// decision.
    #[test]
    fn s06_enforce_quota_window_fields_do_not_affect_decision() {
        let mut result = QuotaCheckResultView::denied("monthly quota exhausted");
        result.window = QuotaWindowView {
            start: Some("2023-11-14T22:13:20Z".to_string()),
            end: Some("2023-11-15T22:13:20Z".to_string()),
            end_inclusive: true,
        };

        let err = match enforce_quota(&result) {
            Ok(()) => panic!("denied result should error"),
            Err(err) => err,
        };

        assert_eq!(err.message, "monthly quota exhausted");
        assert_eq!(err.error_type(), QUOTA_EXCEEDED_CODE);
    }

    // =========================================================================
    // S11 — injectPrompts tests (mirror Go `prompt_test.go` golden cases)
    // =========================================================================

    use conduit_core::objects::prompt::{
        PromptActivationCondition, PromptActivationConditionComposite, PromptSettings, action_type,
        activation_condition_type,
    };
    use conduit_core::objects::prompt_protection::{
        PROMPT_PROTECTION_ACTION_MASK, PROMPT_PROTECTION_ACTION_REJECT, PromptProtectionSettings,
    };
    use conduit_services::{
        Prompt as ServicePrompt, PromptRule, PromptRuleAction, PromptRuleStatus, PromptStatus,
    };

    /// Read the chat payload's messages for assertions (test-only). Panics if
    /// the payload is not the chat variant.
    fn chat_messages_ref(req: &LlmRequest) -> &[conduit_llm::ChatMessage] {
        match &req.payload {
            LlmRequestPayload::Chat(c) => &c.messages,
            _ => panic!("payload must be chat"),
        }
    }

    /// Build a `user` message carrying single-text content (mirrors Go's
    /// `{Role: "user", Content: llm.MessageContent{Content: &userContent}}`).
    fn user_text_message(text: &str) -> conduit_llm::ChatMessage {
        conduit_llm::ChatMessage {
            role: "user".to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        }
    }

    /// Build a chat-payload [`LlmRequest`] carrying the given messages. Mirrors
    /// the Go `&llm.Request{ Model: ..., Messages: ... }` test fixture shape.
    fn llm_with_messages(model: &str, messages: Vec<conduit_llm::ChatMessage>) -> LlmRequest {
        let mut req = llm_request(false);
        req.model = Some(model.to_string());
        req.payload = LlmRequestPayload::Chat(ChatRequest {
            messages,
            ..ChatRequest::default()
        });
        req
    }

    /// Build an enabled `system` prompt with `prepend` action and no conditions
    /// (mirrors the simplest Go fixture in `TestInjectPrompts_WithMatchingPrompts`).
    fn system_prepend_prompt(id: &str, content: &str) -> ServicePrompt {
        ServicePrompt::new(id, id, "project-1", PromptStatus::Active, 0, content)
            .with_role("system")
            .with_settings(PromptSettings {
                action: conduit_core::objects::prompt::PromptAction {
                    kind: action_type::PREPEND.to_string(),
                },
                conditions: Vec::new(),
            })
    }

    /// Build an enabled `system` prompt with the given action kind and
    /// activation conditions (mirrors the Go
    /// `TestInjectPrompts_WithModelCondition` / `_PrependAndAppend` fixtures).
    fn system_prompt_with(
        id: &str,
        content: &str,
        action_kind: &str,
        conditions: Vec<PromptActivationConditionComposite>,
    ) -> ServicePrompt {
        ServicePrompt::new(id, id, "project-1", PromptStatus::Active, 0, content)
            .with_role("system")
            .with_settings(PromptSettings {
                action: conduit_core::objects::prompt::PromptAction {
                    kind: action_kind.to_string(),
                },
                conditions,
            })
    }

    /// Mirrors Go `TestInjectPrompts_NoProjectID`: the wiring layer's "no
    /// project in context" branch surfaces here as "no enabled prompts supplied"
    /// (the wiring would have short-circuited before calling
    /// [`apply_inject_prompts`]). The bridge must leave the request untouched.
    #[test]
    fn s11_no_enabled_prompts_is_skipped_noop() {
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hello")]);

        let outcome = apply_inject_prompts(&mut request, &[], "gpt-4", 0);

        assert_eq!(outcome, InjectPromptsOutcome::Skipped { enabled_count: 0 });
        assert_eq!(chat_messages_ref(&request).len(), 1);
        assert_eq!(chat_messages_ref(&request)[0].role, "user");
    }

    /// Mirrors Go `TestInjectPrompts_WithMatchingPrompts`: a single matching
    /// `prepend` prompt is injected before the original user message.
    #[test]
    fn s11_matching_prepend_prompt_is_injected_before_messages() {
        let prompts = vec![system_prepend_prompt("1", "You are a helpful assistant.")];
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hello")]);

        let outcome = apply_inject_prompts(&mut request, &prompts, "gpt-4", 0);

        let matched = match outcome {
            InjectPromptsOutcome::Injected { matched } => matched,
            other => panic!("expected Injected, got {other:?}"),
        };
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].role, "system");
        assert_eq!(matched[0].content, "You are a helpful assistant.");
        assert_eq!(matched[0].action, "prepend");

        let messages = chat_messages_ref(&request);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        // The injected prompt carries the configured content.
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => {
                assert_eq!(t, "You are a helpful assistant.");
            }
            other => panic!("unexpected injected content: {other:?}"),
        }
    }

    /// Mirrors Go `TestInjectPrompts_WithModelCondition`: only the prompt whose
    /// activation conditions match the request model survives filtering.
    #[test]
    fn s11_model_condition_filters_which_prompt_is_injected() {
        let gpt4_condition = PromptActivationConditionComposite {
            conditions: vec![PromptActivationCondition {
                kind: activation_condition_type::MODEL_ID.to_string(),
                model_id: Some("gpt-4".to_string()),
                model_pattern: None,
                api_key_id: None,
            }],
        };
        let claude_condition = PromptActivationConditionComposite {
            conditions: vec![PromptActivationCondition {
                kind: activation_condition_type::MODEL_PATTERN.to_string(),
                model_id: None,
                model_pattern: Some("claude-.*".to_string()),
                api_key_id: None,
            }],
        };

        let prompts = vec![
            system_prompt_with(
                "1",
                "GPT-4 specific prompt",
                action_type::PREPEND,
                vec![gpt4_condition],
            ),
            system_prompt_with(
                "2",
                "Claude specific prompt",
                action_type::PREPEND,
                vec![claude_condition],
            ),
        ];

        // gpt-4 request -> only prompt 1 matches.
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hello")]);
        let outcome = apply_inject_prompts(&mut request, &prompts, "gpt-4", 0);
        match &outcome {
            InjectPromptsOutcome::Injected { matched } => assert_eq!(matched.len(), 1),
            other => panic!("expected Injected, got {other:?}"),
        }
        let messages = chat_messages_ref(&request);
        assert_eq!(messages.len(), 2);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "GPT-4 specific prompt"),
            other => panic!("unexpected content: {other:?}"),
        }

        // claude request -> only prompt 2 matches.
        let mut request = llm_with_messages("claude-3-opus", vec![user_text_message("Hello")]);
        let outcome = apply_inject_prompts(&mut request, &prompts, "claude-3-opus", 0);
        match &outcome {
            InjectPromptsOutcome::Injected { matched } => assert_eq!(matched.len(), 1),
            other => panic!("expected Injected, got {other:?}"),
        }
        let messages = chat_messages_ref(&request);
        assert_eq!(messages.len(), 2);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "Claude specific prompt"),
            other => panic!("unexpected content: {other:?}"),
        }

        // unknown model -> no prompt matches (Skipped).
        let mut request = llm_with_messages("unknown-model", vec![user_text_message("Hello")]);
        let outcome = apply_inject_prompts(&mut request, &prompts, "unknown-model", 0);
        assert_eq!(outcome, InjectPromptsOutcome::Skipped { enabled_count: 2 });
        assert_eq!(chat_messages_ref(&request).len(), 1);
    }

    /// Mirrors Go `TestInjectPrompts_PrependAndAppend`: prepend prompts land
    /// before, append prompts land after the original messages.
    #[test]
    fn s11_prepend_and_append_land_in_correct_buckets() {
        let prompts = vec![
            system_prompt_with("1", "Prepend prompt", action_type::PREPEND, Vec::new()),
            system_prompt_with("2", "Append prompt", action_type::APPEND, Vec::new()),
        ];
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hello")]);

        let outcome = apply_inject_prompts(&mut request, &prompts, "gpt-4", 0);

        let matched = match outcome {
            InjectPromptsOutcome::Injected { matched } => matched,
            other => panic!("expected Injected, got {other:?}"),
        };
        // Two prompts survive; their reasons preserve the apply order
        // (prepend first, then append) — mirrors the Go `newMessages` slice.
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].action, "prepend");
        assert_eq!(matched[1].action, "append");

        let messages = chat_messages_ref(&request);
        assert_eq!(messages.len(), 3);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "Prepend prompt"),
            other => panic!("unexpected content[0]: {other:?}"),
        }
        match &messages[1].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "Hello"),
            other => panic!("unexpected content[1]: {other:?}"),
        }
        match &messages[2].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "Append prompt"),
            other => panic!("unexpected content[2]: {other:?}"),
        }
    }

    /// Mirrors the api-key-activation branch of Go `matchCondition`
    /// (`PromptActivationConditionTypeAPIKey`): a prompt gated on `api_key_id`
    /// only matches when the request carries that exact id.
    #[test]
    fn s11_api_key_condition_filters_by_request_api_key_id() {
        let condition = PromptActivationConditionComposite {
            conditions: vec![PromptActivationCondition {
                kind: activation_condition_type::API_KEY.to_string(),
                model_id: None,
                model_pattern: None,
                api_key_id: Some(42),
            }],
        };
        let prompts = vec![system_prompt_with(
            "1",
            "key-gated",
            action_type::PREPEND,
            vec![condition],
        )];

        // Wrong api key -> Skipped.
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hi")]);
        let outcome = apply_inject_prompts(&mut request, &prompts, "gpt-4", 7);
        assert!(matches!(outcome, InjectPromptsOutcome::Skipped { .. }));
        assert_eq!(chat_messages_ref(&request).len(), 1);

        // Matching api key -> Injected.
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hi")]);
        let outcome = apply_inject_prompts(&mut request, &prompts, "gpt-4", 42);
        assert!(matches!(outcome, InjectPromptsOutcome::Injected { .. }));
        assert_eq!(chat_messages_ref(&request).len(), 2);
    }

    // =========================================================================
    // S12 — protectPrompts tests (mirror Go `prompt_protection_test.go`)
    // =========================================================================

    /// Build a mask rule that replaces `pattern` matches with `replacement`
    /// across the supplied scopes (empty scopes => applies to all roles, per
    /// Go `promptProtectionRuleAppliesToRole`).
    fn mask_rule(
        id: &str,
        name: &str,
        pattern: &str,
        replacement: &str,
        scopes: Vec<String>,
    ) -> (PromptRule, PromptProtectionSettings) {
        let rule = PromptRule::new(
            id,
            name,
            "project-1",
            PromptRuleStatus::Enabled,
            0,
            pattern,
            // PromptRuleAction on the rule entity is independent of the
            // PromptProtectionSettings.action (mask/reject); the wiring layer
            // derives the latter from rule.Settings. We use Allow here because
            // the entity-level action is the allow/block list semantics, while
            // Settings.action controls the masking/rejection behavior.
            PromptRuleAction::Allow,
        );
        let settings = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_MASK.to_string(),
            replacement: Some(replacement.to_string()),
            scopes,
        };
        (rule, settings)
    }

    /// Build a reject rule.
    fn reject_rule(id: &str, name: &str, pattern: &str) -> (PromptRule, PromptProtectionSettings) {
        let rule = PromptRule::new(
            id,
            name,
            "project-1",
            PromptRuleStatus::Enabled,
            0,
            pattern,
            PromptRuleAction::Block,
        );
        let settings = PromptProtectionSettings {
            action: PROMPT_PROTECTION_ACTION_REJECT.to_string(),
            replacement: None,
            scopes: Vec::new(),
        };
        (rule, settings)
    }

    /// Mirrors Go `TestProtectPromptsMaskContent`: a `mask` rule rewrites the
    /// matching single-text content in place; the *original* request passed by
    /// the caller is observably untouched (Go asserts `request` still has the
    /// unmasked text because `Protect` returns a new `*llm.Request`).
    ///
    /// In the Rust port [`apply_protect_prompts`] mutates the [`LlmRequest`] in
    /// place, so we cannot assert the caller's slice is untouched — but we *can*
    /// assert the masked result lands on the request and that only the
    /// matching text was rewritten (mirroring the Go observable effect on the
    /// returned `protected` request).
    #[test]
    fn s12_mask_rule_rewrites_single_text_content_in_place() {
        let (rule, settings) =
            mask_rule("r-1", "mask-secret", r"secret-\d+", "[MASKED]", Vec::new());
        let rules: &[(&PromptRule, PromptProtectionSettings)] = &[(&rule, settings)];

        let mut request =
            llm_with_messages("gpt-4", vec![user_text_message("token is secret-123")]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match outcome {
            ProtectOutcome::Allow { masked_by } => {
                assert_eq!(masked_by, vec!["r-1".to_string()]);
            }
            other => panic!("expected Allow, got {other:?}"),
        }

        let messages = chat_messages_ref(&request);
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "token is [MASKED]"),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Mirrors Go `TestProtectPromptsRejectContent`: a `reject` rule short-
    /// circuits and the request is rejected. The Go middleware wraps the error
    /// as `transformer.ErrInvalidRequest: <promptProtectionRejectedMessage>` and
    /// ensures the underlying rejection detail (Go's sentinel error string
    /// `"reject-secret"`) is *not* leaked — we mirror both checks via
    /// [`ProtectOutcome::to_rejection_error`].
    #[test]
    fn s12_reject_rule_blocks_and_surfaces_go_shaped_error() {
        let (rule, settings) = reject_rule("r-reject", "reject-secret", r"secret");
        let rules: &[(&PromptRule, PromptProtectionSettings)] = &[(&rule, settings)];

        let mut request = llm_with_messages("gpt-4", vec![user_text_message("contains secret")]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match &outcome {
            ProtectOutcome::Block {
                rejecting_rule_id,
                rejecting_rule_name,
            } => {
                assert_eq!(*rejecting_rule_id, "r-reject");
                assert_eq!(*rejecting_rule_name, "reject-secret");
            }
            other => panic!("expected Block, got {other:?}"),
        }

        // The Go middleware emits `transformer.ErrInvalidRequest:
        // <promptProtectionRejectedMessage>`. Verify the Rust bridge surfaces
        // the same shape via `to_rejection_error`.
        let err = outcome.to_rejection_error();
        assert_eq!(err.error_type(), "invalid_request");
        assert_eq!(err.message, PROMPT_PROTECTION_REJECTED_MESSAGE);
        assert_eq!(err.public_message(), PROMPT_PROTECTION_REJECTED_MESSAGE);
        // The underlying rejection sentinel (`"reject-secret"`) must NOT leak —
        // Go asserts `assert.NotContains(t, err.Error(), "reject-secret")`.
        assert!(!err.message.contains("reject-secret"));
    }

    /// Mirrors Go `TestProtectPromptsScopeFiltering`: a rule with scopes that do
    /// not include the message's role does not fire, so the request is admitted
    /// unchanged. (The Go fixture uses an `assistant` message; here we gate the
    /// rule to `system` scope and pass a `user` message to exercise the same
    /// skip path.)
    #[test]
    fn s12_rule_skipped_when_role_not_in_scopes_admits_unchanged() {
        let (rule, settings) = mask_rule(
            "r-sys-only",
            "system-mask",
            r"secret",
            "[MASKED]",
            // Only `system` messages are in scope.
            vec!["system".to_string()],
        );
        let rules: &[(&PromptRule, PromptProtectionSettings)] = &[(&rule, settings)];

        let mut request = llm_with_messages("gpt-4", vec![user_text_message("contains secret")]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match outcome {
            ProtectOutcome::Allow { masked_by } => {
                assert!(masked_by.is_empty(), "no rule should fire: {masked_by:?}");
            }
            other => panic!("expected Allow with no matches, got {other:?}"),
        }

        // The user message text is unchanged.
        let messages = chat_messages_ref(&request);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "contains secret"),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Mirrors Go `TestProtectPromptsMaskMultipleContent`: a `mask` rule
    /// rewrites the matching `text`-typed content part of a multi-part message.
    #[test]
    fn s12_mask_rule_rewrites_multi_part_text_content() {
        let (rule, settings) =
            mask_rule("r-multi", "mask-multi", r"secret", "[MASKED]", Vec::new());
        let rules: &[(&PromptRule, PromptProtectionSettings)] = &[(&rule, settings)];

        let part = conduit_llm::ContentPart {
            part_type: "text".to_string(),
            text: Some("secret text".to_string()),
            image_url: None,
            input_audio: None,
            extra: BTreeMap::new(),
        };
        let multi_message = conduit_llm::ChatMessage {
            role: "user".to_string(),
            name: None,
            content: Some(conduit_llm::MessageContent::Parts(vec![part])),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: BTreeMap::new(),
        };
        let mut request = llm_with_messages("gpt-4", vec![multi_message]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match outcome {
            ProtectOutcome::Allow { masked_by } => {
                assert_eq!(masked_by, vec!["r-multi".to_string()]);
            }
            other => panic!("expected Allow, got {other:?}"),
        }

        let messages = chat_messages_ref(&request);
        match &messages[0].content {
            Some(conduit_llm::MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                match &parts[0].text {
                    Some(t) => assert_eq!(t, "[MASKED] text"),
                    None => panic!("text should be present after mask"),
                }
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Mirrors the Go `if len(rules) == 0 { return req, nil }` fast path (the
    /// wiring layer's "no PromptProtecter / no enabled rules" branch surfaces
    /// here as an empty `rules` slice).
    #[test]
    fn s12_no_rules_admits_unchanged() {
        let mut request = llm_with_messages("gpt-4", vec![user_text_message("anything")]);

        let outcome = apply_protect_prompts(&mut request, &[]);

        assert_eq!(
            outcome,
            ProtectOutcome::Allow {
                masked_by: Vec::new()
            }
        );
        // Request untouched.
        match &chat_messages_ref(&request)[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "anything"),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Mirrors Go `Protect`'s "no rule matched" branch
    /// (`len(result.MatchedRules) == 0`): the request is admitted unchanged
    /// with an empty `masked_by` list.
    #[test]
    fn s12_rule_does_not_match_admits_unchanged() {
        let (rule, settings) = mask_rule(
            "r-no-match",
            "never-matches",
            r"this-exact-string-will-not-appear",
            "[MASKED]",
            Vec::new(),
        );
        let rules: &[(&PromptRule, PromptProtectionSettings)] = &[(&rule, settings)];

        let mut request = llm_with_messages("gpt-4", vec![user_text_message("Hello world")]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match outcome {
            ProtectOutcome::Allow { masked_by } => assert!(masked_by.is_empty()),
            other => panic!("expected Allow with no matches, got {other:?}"),
        }
        match &chat_messages_ref(&request)[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "Hello world"),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Mirrors the Go `reject` short-circuit on the *first* rejecting rule:
    /// when a `reject` rule fires, no later `mask` rule is evaluated (Go returns
    /// `PromptProtectionResult{MatchedRules: []*ent.PromptProtectionRule{rule},
    /// Rejected: true}` immediately). We assert the Block outcome carries only
    /// the rejecting rule and that the masked-by list of a following mask rule
    /// does not appear.
    #[test]
    fn s12_reject_short_circuits_before_later_mask_rule() {
        // NOTE: `preview_protection` evaluates rules in the slice order; put the
        // reject rule first so it fires before the mask rule would have run.
        let (reject_r, reject_s) = reject_rule("r-reject", "reject-first", r"forbidden");
        let (mask_r, mask_s) =
            mask_rule("r-mask", "mask-after", r"forbidden", "[MASKED]", Vec::new());
        let rules: &[(&PromptRule, PromptProtectionSettings)] =
            &[(&reject_r, reject_s), (&mask_r, mask_s)];

        let mut request =
            llm_with_messages("gpt-4", vec![user_text_message("has forbidden token")]);

        let outcome = apply_protect_prompts(&mut request, rules);

        match outcome {
            ProtectOutcome::Block {
                rejecting_rule_id,
                rejecting_rule_name,
            } => {
                assert_eq!(rejecting_rule_id, "r-reject");
                assert_eq!(rejecting_rule_name, "reject-first");
            }
            other => panic!("expected Block, got {other:?}"),
        }

        // Request is left untouched (reject short-circuits before mutation).
        match &chat_messages_ref(&request)[0].content {
            Some(conduit_llm::MessageContent::Text(t)) => assert_eq!(t, "has forbidden token"),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    /// Outcome predicate coverage — `is_allow` / `is_block` are part of the
    /// bridge's public surface (used by the wiring layer).
    #[test]
    fn s12_outcome_predicates_classify_allow_and_block() {
        let allow = ProtectOutcome::Allow {
            masked_by: vec!["r".to_string()],
        };
        let block = ProtectOutcome::Block {
            rejecting_rule_id: "r".to_string(),
            rejecting_rule_name: "n".to_string(),
        };
        assert!(allow.is_allow());
        assert!(!allow.is_block());
        assert!(block.is_block());
        assert!(!block.is_allow());
    }

    // =========================================================================
    // S20 — withPerformanceRecording tests (mirror Go `performance_test.go`)
    // =========================================================================

    /// Mirrors Go `TestPerformanceRecording_OnInboundLlmRequest_SetsStreamFlag`:
    /// the inbound hook's only job is to set `state.Perf.Stream` from the
    /// request. The pure plan surfaces this as `RecordingPlan::for_inbound(stream)`.
    /// Go has three sub-cases: stream=true, stream=false, stream=nil (defaults
    /// to false). In Rust `stream` is a `bool`, so the nil case is mapped to
    /// `false` by the wiring layer — we cover all three observable outcomes.
    #[test]
    fn s20_inbound_plan_carries_stream_flag_from_request() {
        // stream=true (Go: `streamValue = new(true)`).
        let plan = RecordingPlan::for_inbound(true);
        assert!(plan.stream, "stream=true must propagate to plan.stream");

        // stream=false (Go: `streamValue = new(false)`).
        let plan = RecordingPlan::for_inbound(false);
        assert!(!plan.stream, "stream=false must propagate to plan.stream");

        // stream=nil → defaults to false (Go's `else { Stream = false }` branch).
        // The wiring layer passes `false`; we verify the plan is consistent.
        let plan = RecordingPlan::for_inbound(false);
        assert!(
            !plan.stream,
            "nil-stream (mapped to false) must set plan.stream=false"
        );
    }

    /// Mirrors Go
    /// `TestPerformanceRecording_OnOutboundRawRequest_PreservesStreamFlag` /
    /// `_StreamFlagBugRegression`: the stream flag set on inbound must survive
    /// the creation of a fresh `PerformanceRecord` on outbound. In the pure port
    /// the plan is built once (on inbound) and read on outbound, so the
    /// preservation is structural — verify the same `RecordingPlan` carries the
    /// flag end-to-end.
    #[test]
    fn s20_plan_preserves_stream_flag_across_lifecycle() {
        // Inbound sets the flag.
        let inbound_plan = RecordingPlan::for_inbound(true);
        // Outbound consumes it (the wiring layer reads `plan.stream` when
        // building the new PerformanceRecord). The plan is immutable, so the
        // value the wiring reads on outbound is identical to the one set on
        // inbound — the bug from Go commit 8afd95c3 is structurally impossible.
        assert!(
            inbound_plan.stream,
            "Stream flag must survive inbound→outbound"
        );

        let inbound_plan = RecordingPlan::for_inbound(false);
        assert!(!inbound_plan.stream);
    }

    /// Mirrors Go `TestPerformanceRecording_OnOutboundRawRequest_NoChannel`:
    /// when there is no channel, OnOutboundRawRequest returns early and leaves
    /// `state.Perf` untouched. The pure equivalent: the wiring layer skips
    /// building a fresh plan and reuses the inbound plan. The inbound plan's
    /// `stream` flag is therefore what the wiring continues to see. Verify the
    /// plan is unmodified when the wiring decides to skip.
    #[test]
    fn s20_inbound_plan_is_reused_when_no_channel_on_outbound() {
        let plan = RecordingPlan::for_inbound(true);
        // Wiring layer's "no channel" branch: do not rebuild the plan.
        // The same plan is observed downstream.
        assert!(plan.stream);
    }

    /// Mirrors Go `recordPerformanceStream.Current`'s `MarkFirstToken`
    /// transition: on the very first event of any kind, `MarkFirstToken` fires
    /// exactly once. Subsequent events do not re-fire it.
    #[test]
    fn s20_first_token_fires_once_on_first_stream_event_then_never_again() {
        let plan = RecordingPlan::for_inbound(true);
        let initial = MarkerFiredState::default();

        // First event (any kind) → FirstToken fires.
        let (marker, state) = plan.stream_marker(StreamEventKind::Other, initial);
        assert_eq!(marker, Some(RecordingMarker::FirstToken));
        assert!(state.first_token);

        // Second event → FirstToken does NOT fire again (already set).
        let (marker, state_after) = plan.stream_marker(StreamEventKind::Other, state);
        assert_eq!(marker, None, "FirstToken must not fire twice");
        assert_eq!(state_after, state);
    }

    /// Mirrors Go `recordPerformanceStream.Current`'s reasoning markers: a
    /// `Reasoning` event fires `MarkReasoningStart`, and the first subsequent
    /// `Content` event fires `MarkReasoningEnd`.
    #[test]
    fn s20_reasoning_start_then_end_transitions_in_order() {
        let plan = RecordingPlan::for_inbound(true);

        // Event 1: any event → FirstToken.
        let (m1, s1) = plan.stream_marker(StreamEventKind::Other, MarkerFiredState::default());
        assert_eq!(m1, Some(RecordingMarker::FirstToken));

        // Event 2: reasoning → ReasoningStart (FirstToken already fired, so it's
        // the only marker this event).
        let (m2, s2) = plan.stream_marker(StreamEventKind::Reasoning, s1);
        assert_eq!(m2, Some(RecordingMarker::ReasoningStart));
        assert!(s2.reasoning_start);

        // Event 3: content → ReasoningEnd (reasoning had started, not ended yet).
        let (m3, s3) = plan.stream_marker(StreamEventKind::Content, s2);
        assert_eq!(m3, Some(RecordingMarker::ReasoningEnd));
        assert!(s3.reasoning_end);

        // Event 4: more content → no further markers.
        let (m4, _) = plan.stream_marker(StreamEventKind::Content, s3);
        assert_eq!(m4, None);
    }

    /// Mirrors Go `recordPerformanceStream.Current`'s usage marker: a usage
    /// chunk with non-zero completion tokens fires `MarkSuccess` on the stream
    /// (Go: `s.state.Perf.MarkSuccess(); s.state.ChannelService.AsyncRecordPerformance`).
    #[test]
    fn s20_usage_event_with_nonzero_tokens_records_success() {
        let plan = RecordingPlan::for_inbound(true);

        let (m1, s1) = plan.stream_marker(StreamEventKind::Other, MarkerFiredState::default());
        assert_eq!(m1, Some(RecordingMarker::FirstToken));

        let (m2, _s2) = plan.stream_marker(StreamEventKind::Usage, s1);
        assert_eq!(m2, Some(RecordingMarker::RecordSuccess));
    }

    /// Mirrors Go `recordPerformanceStream.Current`'s "content before reasoning
    /// never fires ReasoningEnd" branch: ReasoningEnd only fires when
    /// ReasoningStart has already fired.
    #[test]
    fn s20_content_without_prior_reasoning_does_not_fire_reasoning_end() {
        let plan = RecordingPlan::for_inbound(true);

        let (m1, s1) = plan.stream_marker(StreamEventKind::Other, MarkerFiredState::default());
        assert_eq!(m1, Some(RecordingMarker::FirstToken));

        // Content without prior reasoning — no marker.
        let (m2, s2) = plan.stream_marker(StreamEventKind::Content, s1);
        assert_eq!(m2, None);
        assert!(!s2.reasoning_end);
    }

    /// Mirrors Go's stream-skip path: non-stream attempts never fire per-event
    /// markers — they go straight to the terminal marker via
    /// `OnOutboundLlmResponse`.
    #[test]
    fn s20_non_stream_plan_never_fires_per_event_markers() {
        let plan = RecordingPlan::for_inbound(false);

        for kind in [
            StreamEventKind::Reasoning,
            StreamEventKind::Content,
            StreamEventKind::Usage,
            StreamEventKind::Other,
        ] {
            let (marker, state) = plan.stream_marker(kind, MarkerFiredState::default());
            assert_eq!(
                marker, None,
                "non-stream plan must not fire marker for {kind:?}"
            );
            assert_eq!(
                state,
                MarkerFiredState::default(),
                "state must stay default"
            );
        }
    }

    /// `classify_stream_event` mirrors Go's `recordPerformanceStream.Current`
    /// delta-discrimination order.
    #[test]
    fn s20_classify_stream_event_priority_reasoning_then_usage_then_content() {
        // Reasoning takes priority.
        assert_eq!(
            classify_stream_event(true, true, true),
            StreamEventKind::Reasoning
        );
        // Then usage.
        assert_eq!(
            classify_stream_event(false, true, true),
            StreamEventKind::Usage
        );
        // Then content.
        assert_eq!(
            classify_stream_event(false, true, false),
            StreamEventKind::Content
        );
        // Otherwise Other.
        assert_eq!(
            classify_stream_event(false, false, false),
            StreamEventKind::Other
        );
    }

    /// Mirrors Go `OnOutboundLlmResponse` (MarkSuccess), `OnOutboundRawError`
    /// with `context.Canceled` (MarkCanceled), and `OnOutboundRawError` with a
    /// generic error (MarkFailed + ExtractErrorCode).
    #[test]
    fn s20_terminal_marker_success_canceled_failed_precedence() {
        // Success path.
        assert_eq!(terminal_marker(true, false, 0), TerminalMarker::Success);

        // Canceled beats failed.
        assert_eq!(terminal_marker(false, true, 0), TerminalMarker::Canceled);

        // Failed with extracted code.
        assert_eq!(
            terminal_marker(false, false, 503),
            TerminalMarker::Failed { error_code: 503 }
        );

        // Failed with zero code defaults to 500 (Go ExtractErrorCode default).
        assert_eq!(
            terminal_marker(false, false, 0),
            TerminalMarker::Failed { error_code: 500 }
        );
    }

    // =========================================================================
    // S21 — withModelCircuitBreaker tests
    // =========================================================================

    /// Build an Open-state snapshot for tests.
    fn open_stats(channel: &str, model: &str) -> ModelCircuitBreakerStatsView {
        ModelCircuitBreakerStatsView {
            channel_id: channel.to_string(),
            model_id: model.to_string(),
            state: CircuitBreakerStateView::Open,
            next_probe_at: Some("2024-01-01T00:00:00Z".to_string()),
            probing_in_progress: false,
        }
    }

    /// Build a Closed-state snapshot for tests.
    fn closed_stats(channel: &str, model: &str) -> ModelCircuitBreakerStatsView {
        ModelCircuitBreakerStatsView::closed(channel, model)
    }

    /// Mirrors Go branch `(a)`: `strategy != biz.LoadBalancerStrategyCircuitBreaker`
    /// → Allow(FeatureDisabled). Also covers the `modelCircuitBreaker == nil`
    /// half of the condition (the wiring layer collapses both into
    /// `strategy_enabled == false`).
    #[test]
    fn s21_strategy_disabled_admits_with_feature_disabled_reason() {
        let stats = open_stats("ch-1", "gpt-4");

        let decision = check_model_circuit_breaker(
            false, // strategy != circuit-breaker (or CB is nil)
            true,
            Some(&stats),
            ProbeEligibility::default(),
        );

        assert_eq!(
            decision,
            CircuitBreakerDecision::Allow {
                reason: AllowReason::FeatureDisabled
            }
        );
    }

    /// Mirrors Go branch `(b)`: `channel == nil || modelID == ""` →
    /// Allow(NoTarget).
    #[test]
    fn s21_no_target_admits_with_no_target_reason() {
        let stats = open_stats("ch-1", "gpt-4");

        let decision = check_model_circuit_breaker(
            true,
            false, // no channel / no model id
            Some(&stats),
            ProbeEligibility::default(),
        );

        assert_eq!(
            decision,
            CircuitBreakerDecision::Allow {
                reason: AllowReason::NoTarget
            }
        );
    }

    /// Mirrors Go branch `(c)` first half: `stats == nil` → Allow(NotOpen).
    #[test]
    fn s21_nil_stats_admits_with_not_open_reason() {
        let decision = check_model_circuit_breaker(
            true,
            true,
            None, // Go: stats == nil
            ProbeEligibility::default(),
        );

        assert_eq!(
            decision,
            CircuitBreakerDecision::Allow {
                reason: AllowReason::NotOpen
            }
        );
    }

    /// Mirrors Go branch `(c)` second half: `stats.State != StateOpen` →
    /// Allow(NotOpen). Covers both Closed and HalfOpen (Go admits both).
    #[test]
    fn s21_closed_and_half_open_states_admit_with_not_open_reason() {
        let closed = closed_stats("ch-1", "gpt-4");
        let decision =
            check_model_circuit_breaker(true, true, Some(&closed), ProbeEligibility::default());
        assert_eq!(
            decision,
            CircuitBreakerDecision::Allow {
                reason: AllowReason::NotOpen
            }
        );

        let mut half_open = closed_stats("ch-1", "gpt-4");
        half_open.state = CircuitBreakerStateView::HalfOpen;
        let decision =
            check_model_circuit_breaker(true, true, Some(&half_open), ProbeEligibility::default());
        assert_eq!(
            decision,
            CircuitBreakerDecision::Allow {
                reason: AllowReason::NotOpen
            }
        );
    }

    /// Mirrors Go branches `(d)` + `(e)`: state Open + probe granted →
    /// HalfOpenProbe. Go's `TryBeginProbe` returns `true` only when
    /// `NextProbeAt` is in the past AND no probe is in flight — the pure
    /// decision consumes those as `ProbeEligibility`.
    #[test]
    fn s21_open_state_with_eligible_probe_grants_half_open_probe() {
        let stats = open_stats("ch-1", "gpt-4");
        let probe = ProbeEligibility {
            next_probe_at_reached: true,
            no_probe_in_flight: true,
        };

        let decision = check_model_circuit_breaker(true, true, Some(&stats), probe);

        assert_eq!(decision, CircuitBreakerDecision::HalfOpenProbe);
    }

    /// Mirrors Go branch `(d)`: state Open + probe NOT granted → RejectOpen.
    /// Covers the three `TryBeginProbe` rejection paths: probe time not reached,
    /// probe already in flight, and both.
    #[test]
    fn s21_open_state_with_probe_not_granted_rejects() {
        let stats = open_stats("ch-1", "gpt-4");

        // Probe time not reached yet.
        let decision = check_model_circuit_breaker(
            true,
            true,
            Some(&stats),
            ProbeEligibility {
                next_probe_at_reached: false,
                no_probe_in_flight: true,
            },
        );
        assert_eq!(decision, CircuitBreakerDecision::RejectOpen);

        // Another probe is already in flight.
        let decision = check_model_circuit_breaker(
            true,
            true,
            Some(&stats),
            ProbeEligibility {
                next_probe_at_reached: true,
                no_probe_in_flight: false,
            },
        );
        assert_eq!(decision, CircuitBreakerDecision::RejectOpen);

        // Both conditions fail.
        let decision = check_model_circuit_breaker(
            true,
            true,
            Some(&stats),
            ProbeEligibility {
                next_probe_at_reached: false,
                no_probe_in_flight: false,
            },
        );
        assert_eq!(decision, CircuitBreakerDecision::RejectOpen);
    }

    /// The Go sentinel message must match exactly (Go:
    /// `errors.New("skip candidate by circuit breaker")`). Pin the literal so a
    /// future refactor that drifts is caught.
    #[test]
    fn s21_skip_candidate_message_matches_go_literal() {
        assert_eq!(
            SKIP_CANDIDATE_BY_CIRCUIT_BREAKER_MESSAGE,
            "skip candidate by circuit breaker"
        );
        assert_eq!(LOAD_BALANCER_STRATEGY_CIRCUIT_BREAKER, "circuit-breaker");
    }

    /// `CircuitBreakerStateView::as_str` must match the Go encoding
    /// (`"closed"` / `"half_open"` / `"open"`).
    #[test]
    fn s21_state_view_strings_match_go_encoding() {
        assert_eq!(CircuitBreakerStateView::Closed.as_str(), "closed");
        assert_eq!(CircuitBreakerStateView::HalfOpen.as_str(), "half_open");
        assert_eq!(CircuitBreakerStateView::Open.as_str(), "open");
    }

    /// `ProbeEligibility::eligible` is the conjunction of the two flags (mirrors
    /// Go `TryBeginProbe` short-circuit: returns false if either fails).
    #[test]
    fn s21_probe_eligibility_is_conjunction() {
        assert!(
            ProbeEligibility {
                next_probe_at_reached: true,
                no_probe_in_flight: true,
            }
            .eligible()
        );

        assert!(
            !ProbeEligibility {
                next_probe_at_reached: false,
                no_probe_in_flight: true,
            }
            .eligible()
        );

        assert!(
            !ProbeEligibility {
                next_probe_at_reached: true,
                no_probe_in_flight: false,
            }
            .eligible()
        );

        assert!(!ProbeEligibility::default().eligible());
    }

    /// `ModelCircuitBreakerStatsView::closed` mirrors Go's `getStats` default:
    /// fresh entries are created with `State: StateClosed`, no probe scheduled,
    /// no probe in flight.
    #[test]
    fn s21_closed_snapshot_matches_go_default_get_stats() {
        let view = ModelCircuitBreakerStatsView::closed("ch-7", "claude-3");
        assert_eq!(view.channel_id, "ch-7");
        assert_eq!(view.model_id, "claude-3");
        assert_eq!(view.state, CircuitBreakerStateView::Closed);
        assert!(view.next_probe_at.is_none());
        assert!(!view.probing_in_progress);
    }

    // =========================================================================
    // S22 — persistRequestExecution tests (mirror Go
    //   `request_execution.go` + `channel_metrics.go::Calculate` /
    //   `CalculateReasoningDurationMs` / `ClampLatency`)
    // =========================================================================

    /// Pin the Go `biz.MinLatencyMs` literal — Go
    /// (`internal/server/biz/channel_metrics.go`) clamps latencies to this
    /// minimum to prevent extreme TPS calculations. If the Go default ever
    /// drifts this test catches it.
    #[test]
    fn s22_min_latency_constant_matches_go_literal() {
        // Go `biz.MinLatencyMs = 10` (channel_metrics.go:24, "minimum latency value (10ms)").
        assert_eq!(BIZ_MIN_LATENCY_MS, 10);
    }

    /// Mirrors Go `biz.ClampLatency`: values below `MinLatencyMs` are raised to
    /// `MinLatencyMs`; values at or above are returned unchanged. Go's `<`
    /// comparison also covers negatives (clock skew), which we mirror.
    #[test]
    fn s22_clamp_latency_enforces_minimum_like_go() {
        // Above the floor → unchanged.
        assert_eq!(clamp_latency(500), 500);
        assert_eq!(clamp_latency(10), 10);

        // At/below the floor → raised to MinLatencyMs (10).
        assert_eq!(clamp_latency(9), BIZ_MIN_LATENCY_MS);
        assert_eq!(clamp_latency(0), BIZ_MIN_LATENCY_MS);
        assert_eq!(clamp_latency(-5), BIZ_MIN_LATENCY_MS);
    }

    /// Mirrors the `state.Perf == nil || StartTime.IsZero()` short-circuit in
    /// `OnOutboundLlmResponse`: when the wiring reports no request latency
    /// (Go would have skipped the metrics block entirely), the helper returns
    /// `None`.
    #[test]
    fn s22_build_latency_metrics_returns_none_when_no_perf_record() {
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: None,
            first_token_latency_ms: Some(120),
            reasoning_duration_ms: Some(40),
        };

        assert_eq!(build_latency_metrics(&inputs), None);
    }

    /// Mirrors Go's non-stream success path: only `latency_ms` is populated;
    /// first-token and reasoning metrics stay `None` (Go's
    /// `state.Perf.Stream && FirstTokenTime != nil` / `state.Perf.Stream` gates
    /// both fail on `Stream == false`).
    #[test]
    fn s22_build_latency_metrics_non_stream_only_populates_latency() {
        let inputs = LatencyInputs {
            stream: false,
            request_latency_ms: Some(1500),
            // Even if the wiring supplies these (defensive), the helper must
            // drop them because Stream == false.
            first_token_latency_ms: Some(120),
            reasoning_duration_ms: Some(40),
        };

        let metrics = match build_latency_metrics(&inputs) {
            Some(m) => m,
            None => panic!("non-stream success must still build metrics"),
        };

        assert_eq!(metrics.latency_ms, Some(1500));
        assert!(
            metrics.first_token_latency_ms.is_none(),
            "non-stream requests must not carry first-token latency"
        );
        assert!(
            metrics.reasoning_duration_ms.is_none(),
            "non-stream requests must not carry reasoning duration"
        );
    }

    /// Mirrors Go's streaming success path with a first-token event: latency,
    /// first-token, and reasoning duration are all populated (when supplied).
    #[test]
    fn s22_build_latency_metrics_stream_with_first_token_populates_all() {
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: Some(2500),
            first_token_latency_ms: Some(300),
            reasoning_duration_ms: Some(800),
        };

        let metrics = build_latency_metrics(&inputs)
            .unwrap_or_else(|| panic!("stream success with latency must build metrics"));

        assert_eq!(metrics.latency_ms, Some(2500));
        assert_eq!(metrics.first_token_latency_ms, Some(300));
        assert_eq!(metrics.reasoning_duration_ms, Some(800));
    }

    /// Mirrors Go's streaming path without a first-token event (e.g. the
    /// upstream returned no SSE events before completion — edge case): only
    /// latency + reasoning are populated; first-token stays `None`.
    #[test]
    fn s22_build_latency_metrics_stream_without_first_token_omits_it() {
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: Some(900),
            first_token_latency_ms: None,
            reasoning_duration_ms: Some(200),
        };

        let metrics = build_latency_metrics(&inputs)
            .unwrap_or_else(|| panic!("stream success with latency must build metrics"));

        assert_eq!(metrics.latency_ms, Some(900));
        assert!(metrics.first_token_latency_ms.is_none());
        assert_eq!(metrics.reasoning_duration_ms, Some(200));
    }

    /// Mirrors Go's clamp behavior inside `Calculate()`: latencies below
    /// `MinLatencyMs` are clamped up to the minimum. Verify the clamp fires on
    /// both `latency_ms` and (for streams) `first_token_latency_ms`.
    #[test]
    fn s22_build_latency_metrics_applies_clamp_to_below_floor_latencies() {
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: Some(0),
            first_token_latency_ms: Some(-3),
            reasoning_duration_ms: Some(50),
        };

        let metrics = build_latency_metrics(&inputs)
            .unwrap_or_else(|| panic!("should still build metrics even when clamping fires"));

        assert_eq!(metrics.latency_ms, Some(BIZ_MIN_LATENCY_MS));
        assert_eq!(metrics.first_token_latency_ms, Some(BIZ_MIN_LATENCY_MS));
        // Reasoning duration is NOT clamped (Go does not ClampLatency it), only
        // gated on `> 0`. 50 is positive → passes through unchanged.
        assert_eq!(metrics.reasoning_duration_ms, Some(50));
    }

    /// Mirrors Go's `if reasoningDurationMs > 0` filter: zero and negative
    /// reasoning durations are dropped (Go `CalculateReasoningDurationMs`
    /// returns 0 when either marker is unset; the `> 0` check then drops it).
    #[test]
    fn s22_build_latency_metrics_drops_non_positive_reasoning_duration() {
        // Zero → dropped.
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: Some(1000),
            first_token_latency_ms: Some(100),
            reasoning_duration_ms: Some(0),
        };
        let metrics =
            build_latency_metrics(&inputs).unwrap_or_else(|| panic!("should build metrics"));
        assert!(
            metrics.reasoning_duration_ms.is_none(),
            "zero reasoning duration must be dropped"
        );

        // Negative → dropped.
        let inputs = LatencyInputs {
            stream: true,
            request_latency_ms: Some(1000),
            first_token_latency_ms: Some(100),
            reasoning_duration_ms: Some(-1),
        };
        let metrics =
            build_latency_metrics(&inputs).unwrap_or_else(|| panic!("should build metrics"));
        assert!(
            metrics.reasoning_duration_ms.is_none(),
            "negative reasoning duration must be dropped"
        );
    }

    /// Mirrors Go `persistRequestExecutionMiddleware.OnOutboundRawRequest`:
    /// the create-plan carries the exact fields Go hands to
    /// `CreateRequestExecution` (channel id, actual model, api format,
    /// pass-through flag).
    #[test]
    fn s22_execution_record_create_plan_carries_go_fields() {
        let plan = ExecutionRecordPlan::create(
            "req-42",
            "channel-7",
            "gpt-4o-2024-08-06",
            "openai_chat_completions",
            true,
        );

        assert_eq!(plan.request_id, "req-42");
        assert_eq!(plan.channel_id, "channel-7");
        assert_eq!(plan.actual_model, "gpt-4o-2024-08-06");
        assert_eq!(plan.api_format, "openai_chat_completions");
        assert!(plan.pass_through_applied);

        // And the non-pass-through variant.
        let plan =
            ExecutionRecordPlan::create("req-43", "ch-1", "claude-3", "anthropic_message", false);
        assert!(!plan.pass_through_applied);
    }

    /// Mirrors Go `ExtractErrorInfo`: returns `None` for non-HTTP errors (Go's
    /// `nil` return), and an `ExecutionErrorInfoView` carrying the status code
    /// for HTTP errors.
    #[test]
    fn s22_extract_error_info_returns_none_for_non_http_error() {
        // Non-HTTP error → Go returns nil.
        assert_eq!(extract_error_info(None), None);
    }

    #[test]
    fn s22_extract_error_info_returns_status_code_for_http_error() {
        // HTTP error → Go returns ExecutionErrorInfo{StatusCode: &code}.
        let info = match extract_error_info(Some(503)) {
            Some(info) => info,
            None => panic!("HTTP error must produce Some(info)"),
        };
        assert_eq!(info.status_code, Some(503));

        // 4xx errors are surfaced too (Go does not filter by status class).
        let info = extract_error_info(Some(400))
            .unwrap_or_else(|| panic!("4xx HTTP error must produce Some(info)"));
        assert_eq!(info.status_code, Some(400));
    }

    // =========================================================================
    // S23 — withLivePreview tests (mirror Go `live_streaming.go`)
    // =========================================================================

    /// Mirrors Go `OnInboundLlmRequest` branch 1: `m.liveStreamRegistry == nil`
    /// → disabled with `NoRegistry`.
    #[test]
    fn s23_live_preview_disabled_when_no_registry() {
        let plan = live_preview_plan(
            false, // no registry
            true,
            true,
            Some("req-1".to_string()),
            Some("exec-1".to_string()),
        );

        assert!(!plan.enabled);
        assert_eq!(
            plan.disabled_reason,
            Some(LivePreviewDisableReason::NoRegistry)
        );
    }

    /// Mirrors Go `OnInboundLlmRequest` branch 2: `request.Stream == nil ||
    /// !*request.Stream` → disabled with `NotStreaming`.
    #[test]
    fn s23_live_preview_disabled_when_request_not_streaming() {
        let plan = live_preview_plan(
            true,
            false, // not streaming
            true,
            Some("req-1".to_string()),
            Some("exec-1".to_string()),
        );

        assert!(!plan.enabled);
        assert_eq!(
            plan.disabled_reason,
            Some(LivePreviewDisableReason::NotStreaming)
        );
    }

    /// Mirrors Go `OnInboundLlmRequest` branch 3:
    /// `StoragePolicyOrDefault(ctx).LivePreview == false` → disabled with
    /// `PolicyDisabled`. Also covers the `m.systemService == nil` half (the
    /// wiring layer collapses both into `live_preview_policy == false`).
    #[test]
    fn s23_live_preview_disabled_when_policy_off() {
        let plan = live_preview_plan(
            true,
            true,
            false, // LivePreview=false (Go default, see biz/system_default.go:5)
            Some("req-1".to_string()),
            Some("exec-1".to_string()),
        );

        assert!(!plan.enabled);
        assert_eq!(
            plan.disabled_reason,
            Some(LivePreviewDisableReason::PolicyDisabled)
        );
    }

    /// Mirrors Go's three-gate pass-through: all gates open → enabled, carrying
    /// the request + execution ids for buffer registration.
    #[test]
    fn s23_live_preview_enabled_when_all_gates_pass() {
        let plan = live_preview_plan(
            true,
            true,
            true,
            Some("req-1".to_string()),
            Some("exec-1".to_string()),
        );

        assert!(plan.enabled);
        assert!(plan.disabled_reason.is_none());
        assert_eq!(plan.request_id.as_deref(), Some("req-1"));
        assert_eq!(plan.request_exec_id.as_deref(), Some("exec-1"));
    }

    /// Mirrors Go `OnOutboundRawRequest`'s `m.state.Request == nil` /
    /// `m.state.RequestExec == nil` guards: an enabled plan may still carry
    /// `None` ids when the corresponding state is missing.
    #[test]
    fn s23_live_preview_enabled_plan_allows_missing_ids() {
        let plan = live_preview_plan(true, true, true, None, None);

        assert!(plan.enabled);
        assert!(plan.request_id.is_none());
        assert!(plan.request_exec_id.is_none());
    }

    /// Mirrors Go `OnOutboundRawStream`: wrapping only fires when enabled AND a
    /// `RequestExec` id is available.
    #[test]
    fn s23_live_preview_wrap_execution_stream_only_when_enabled_with_exec_id() {
        // Disabled → None.
        let plan = LivePreviewPlan::disabled(LivePreviewDisableReason::PolicyDisabled);
        assert_eq!(live_preview_wrap_execution_stream(&plan), None);

        // Enabled but no exec id → None (Go: `m.state.RequestExec == nil`).
        let plan = LivePreviewPlan::enabled(Some("req-1".to_string()), None);
        assert_eq!(live_preview_wrap_execution_stream(&plan), None);

        // Enabled with exec id → Some(id).
        let plan = LivePreviewPlan::enabled(None, Some("exec-9".to_string()));
        assert_eq!(live_preview_wrap_execution_stream(&plan), Some("exec-9"));
    }

    /// Mirrors Go `OnInboundRawStream`: wrapping only fires when enabled AND a
    /// `Request` id is available.
    #[test]
    fn s23_live_preview_wrap_request_stream_only_when_enabled_with_request_id() {
        // Disabled → None.
        let plan = LivePreviewPlan::disabled(LivePreviewDisableReason::NoRegistry);
        assert_eq!(live_preview_wrap_request_stream(&plan), None);

        // Enabled but no request id → None (Go: `m.state.Request == nil`).
        let plan = LivePreviewPlan::enabled(None, Some("exec-1".to_string()));
        assert_eq!(live_preview_wrap_request_stream(&plan), None);

        // Enabled with request id → Some(id).
        let plan = LivePreviewPlan::enabled(Some("req-9".to_string()), None);
        assert_eq!(live_preview_wrap_request_stream(&plan), Some("req-9"));
    }

    // =========================================================================
    // S27/S28 — captureRawProviderResponse / captureRawProviderStream tests
    //   (mirror Go `pass_through.go`'s `isPassThroughEnabled` gating)
    // =========================================================================

    /// Mirrors Go `captureRawProviderResponse` + `captureRawProviderStream`'s
    /// shared `isPassThroughEnabled` gate: when pass-through is enabled the
    /// wiring must capture the **raw** provider response/stream (Go:
    /// `state.RawProviderResponse = response` + the `RawStreamCh` fan-out).
    #[test]
    fn s27s28_capture_plan_returns_raw_when_pass_through_enabled() {
        assert_eq!(capture_plan(true), CapturePlan::Raw);
        assert!(capture_plan(true).is_raw());
        assert!(!capture_plan(true).is_transformed());
    }

    /// Mirrors Go's pass-through-disabled path: the capture middlewares return
    /// the stream/response unchanged, so the wiring captures the
    /// **transformed** response/stream (which `persistRequestExecution` then
    /// persists).
    #[test]
    fn s27s28_capture_plan_returns_transformed_when_pass_through_disabled() {
        assert_eq!(capture_plan(false), CapturePlan::Transformed);
        assert!(capture_plan(false).is_transformed());
        assert!(!capture_plan(false).is_raw());
    }

    /// `CapturePlan` predicates are exhaustive and mutually exclusive — pin
    /// both so a future refactor that drifts is caught.
    #[test]
    fn s27s28_capture_plan_predicates_are_mutually_exclusive() {
        let raw = CapturePlan::Raw;
        assert!(raw.is_raw());
        assert!(!raw.is_transformed());

        let transformed = CapturePlan::Transformed;
        assert!(!transformed.is_raw());
        assert!(transformed.is_transformed());
    }

    // =========================================================================
    // S24 — withChannelLimiter tests
    //   (mirror Go `connection_tracking_test.go`)
    // =========================================================================

    /// Mirrors Go `TestChannelLimiterMiddleware_NoLimitChannelBypasses`:
    /// a channel with no rate-limit configured (Go `manager.GetOrCreate` returns
    /// nil) bypasses the limiter entirely — no slot held, no release path.
    #[test]
    fn s24_no_limiter_configured_bypasses_admission() {
        let decision = channel_limiter_decision(None, false);

        assert!(matches!(decision, LimiterDecision::Bypass));
        assert!(decision.is_admitted_path());
        assert!(!decision.is_rejected());
    }

    /// Mirrors Go soft-mode behavior in
    /// `TestChannelLimiterMiddleware_AcquireAndReleaseOnResponse` /
    /// `_OnceProtection` / `_StreamCloseReleases` / `_RetryReacquireDoesNotLeak`:
    /// soft mode (`queueSize == 0`) *always* admits, even when `in_flight`
    /// already meets or exceeds `max_concurrent`. The limiter only counts — it
    /// never blocks or rejects.
    #[test]
    fn s24_soft_mode_always_admits_even_when_in_flight_meets_capacity() {
        let state = ChannelLimiterStateView {
            max_concurrent: 2,
            queue_size: 0, // soft mode
            in_flight: 2,  // already at capacity
            waiting: 0,
        };

        let decision = channel_limiter_decision(Some(&state), false);

        assert!(matches!(decision, LimiterDecision::Admit));
        assert!(decision.is_admitted_path());

        // Even far past capacity, soft mode still admits — Go's soft branch
        // increments `inFlight` unconditionally and returns nil.
        let state = ChannelLimiterStateView {
            max_concurrent: 2,
            queue_size: 0,
            in_flight: 99,
            waiting: 0,
        };
        assert!(matches!(
            channel_limiter_decision(Some(&state), false),
            LimiterDecision::Admit
        ));
    }

    /// Mirrors Go hard-mode fast-path in
    /// `TestChannelLimiterMiddleware_AcquireAndReleaseOnResponse` (capacity 1,
    /// no in-flight): when capacity is available, the request is admitted
    /// immediately.
    #[test]
    fn s24_hard_mode_admits_when_capacity_available() {
        let state = ChannelLimiterStateView {
            max_concurrent: 2,
            queue_size: 5, // hard mode
            in_flight: 1,  // one slot free
            waiting: 0,
        };

        let decision = channel_limiter_decision(Some(&state), false);

        assert!(matches!(decision, LimiterDecision::Admit));

        // Exactly at capacity-1 → still admitted.
        let state = ChannelLimiterStateView {
            max_concurrent: 2,
            queue_size: 5,
            in_flight: 1,
            waiting: 0,
        };
        assert!(matches!(
            channel_limiter_decision(Some(&state), false),
            LimiterDecision::Admit
        ));
    }

    /// Mirrors Go `TestChannelLimiterMiddleware_QueueFullReturnsTypedError`:
    /// hard mode with capacity saturated AND FIFO queue full rejects with
    /// `QueueFull`. No slot is held — Go asserts `m.current.Load() == nil` after
    /// the failed Acquire.
    #[test]
    fn s24_hard_mode_rejects_queue_full_when_capacity_and_queue_saturated() {
        let state = ChannelLimiterStateView {
            max_concurrent: 1,
            queue_size: 1,
            in_flight: 1, // capacity saturated
            waiting: 1,   // queue saturated
        };

        let decision = channel_limiter_decision(Some(&state), false);

        match decision {
            LimiterDecision::Reject {
                reason: ChannelLimiterRejectionReason::QueueFull,
            } => {}
            other => panic!("expected Reject(QueueFull), got {other:?}"),
        }
        assert!(decision.is_rejected());
        assert!(!decision.is_admitted_path());

        // The reason maps to the matching limiter sentinel (Go
        // `ErrChannelQueueFull`).
        assert_eq!(
            ChannelLimiterRejectionReason::QueueFull.to_limiter_error(),
            crate::channel_limiter::ChannelLimiterError::QueueFull,
        );
        // And to the Go string literal.
        assert_eq!(
            ChannelLimiterRejectionReason::QueueFull.as_str(),
            "queue_full"
        );
    }

    /// Mirrors Go `ErrChannelQueueTimeout` (sentinel from `channel_limiter.go`)
    /// surfaced via `asChannelQueueError` as
    /// `channelQueueReasonTimeout = "queue_timeout"`. The pure helper surfaces
    /// `Reject(QueueTimeout)` only when the wiring layer flags that the
    /// per-channel timeout elapsed *while the request was queued*.
    #[test]
    fn s24_hard_mode_rejects_queue_timeout_when_waiter_timed_out() {
        let state = ChannelLimiterStateView {
            max_concurrent: 1,
            queue_size: 5, // queue has room
            in_flight: 1,  // capacity saturated
            waiting: 1,
        };

        // Without timeout: there's still room → Queue.
        assert!(matches!(
            channel_limiter_decision(Some(&state), false),
            LimiterDecision::Queue { position: 1 }
        ));

        // With timeout flag: the wait is abandoned as QueueTimeout.
        let decision = channel_limiter_decision(Some(&state), true);

        match decision {
            LimiterDecision::Reject {
                reason: ChannelLimiterRejectionReason::QueueTimeout,
            } => {}
            other => panic!("expected Reject(QueueTimeout), got {other:?}"),
        }
        assert!(decision.is_rejected());
        assert_eq!(
            ChannelLimiterRejectionReason::QueueTimeout.to_limiter_error(),
            crate::channel_limiter::ChannelLimiterError::ChannelQueueTimeout,
        );
        assert_eq!(
            ChannelLimiterRejectionReason::QueueTimeout.as_str(),
            "queue_timeout",
        );
    }

    /// Mirrors Go `TestChannelLimiter_HardMode_QueueFull`'s queue-with-room
    /// path: hard mode with capacity saturated but FIFO has room → the request
    /// is enqueued. The wiring layer drives the actual wait.
    #[test]
    fn s24_hard_mode_queues_when_capacity_saturated_but_fifo_has_room() {
        let state = ChannelLimiterStateView {
            max_concurrent: 1,
            queue_size: 5,
            in_flight: 1, // saturated
            waiting: 2,   // two ahead of us; we'd be position 2
        };

        let decision = channel_limiter_decision(Some(&state), false);

        match decision {
            LimiterDecision::Queue { position } => {
                // position mirrors `waiters.Len()` at the enqueue moment (Go
                // appends to the back of the list).
                assert_eq!(position, 2);
            }
            other => panic!("expected Queue, got {other:?}"),
        }
        assert!(!decision.is_admitted_path());
        assert!(!decision.is_rejected());
    }

    /// `LimiterDecision` predicates are consistent: `Bypass`/`Admit` are
    /// admitted-path; `Queue` and `Reject` are not; only `Reject` is rejected.
    #[test]
    fn s24_decision_predicates_are_mutually_exclusive() {
        assert!(LimiterDecision::Bypass.is_admitted_path());
        assert!(!LimiterDecision::Bypass.is_rejected());

        assert!(LimiterDecision::Admit.is_admitted_path());
        assert!(!LimiterDecision::Admit.is_rejected());

        let q = LimiterDecision::Queue { position: 0 };
        assert!(!q.is_admitted_path());
        assert!(!q.is_rejected());

        let r = LimiterDecision::Reject {
            reason: ChannelLimiterRejectionReason::QueueFull,
        };
        assert!(!r.is_admitted_path());
        assert!(r.is_rejected());
    }

    /// `ChannelLimiterStateView::mode` mirrors Go's `hardMode := lim.queueSize
    /// > 0` exactly.
    #[test]
    fn s24_mode_derived_from_queue_size_like_go() {
        let soft = ChannelLimiterStateView {
            max_concurrent: 5,
            queue_size: 0,
            in_flight: 0,
            waiting: 0,
        };
        assert_eq!(
            soft.mode(),
            crate::channel_limiter::ChannelLimiterMode::Soft
        );

        let hard = ChannelLimiterStateView {
            max_concurrent: 5,
            queue_size: 1,
            in_flight: 0,
            waiting: 0,
        };
        assert_eq!(
            hard.mode(),
            crate::channel_limiter::ChannelLimiterMode::Hard
        );
    }

    // =========================================================================
    // S25 — withRateLimitAdmission tests
    //   (mirror Go `rate_limit_admission_test.go`)
    // =========================================================================

    /// Mirrors Go `TestRateLimitAdmission_NoRPMBypasses`: a channel with no
    /// RPM limit configured (`rpm = 0` here, Go `limit == nil || *limit <= 0`)
    /// is admitted *without* consuming a slot — the counter must stay at 0
    /// across multiple attempts.
    #[test]
    fn s25_no_rpm_limit_bypasses_admission_without_consuming_slot() {
        let rpm = RpmView {
            requests: 0,
            limit: 0, // no RPM limit
        };

        for _ in 0..3 {
            let decision = rate_limit_admission_decision(&rpm);
            match decision {
                AdmissionDecision::Allow { consumed_slot } => {
                    assert!(
                        !consumed_slot,
                        "no-limit path must NOT consume a slot (Go returns nil without touching the counter)"
                    );
                }
                other => panic!("expected Allow, got {other:?}"),
            }
            assert!(decision.is_allow());
        }
    }

    /// Mirrors Go `TestRateLimitAdmission_AllowsOnlyConfiguredRPM`: a channel
    /// with RPM=2 admits the first two requests (consuming a slot each), then
    /// rejects the third with `RevokeRpm`. The counter advances to 2.
    #[test]
    fn s25_admits_only_configured_rpm_then_rejects() {
        let mut requests: i64 = 0;
        let limit: i64 = 2;

        // First attempt: requests=0 < limit=2 → Allow + consume.
        let rpm = RpmView { requests, limit };
        match rate_limit_admission_decision(&rpm) {
            AdmissionDecision::Allow { consumed_slot } => assert!(consumed_slot),
            other => panic!("expected Allow on attempt 1, got {other:?}"),
        }
        requests += 1;

        // Second attempt: requests=1 < limit=2 → Allow + consume.
        let rpm = RpmView { requests, limit };
        match rate_limit_admission_decision(&rpm) {
            AdmissionDecision::Allow { consumed_slot } => assert!(consumed_slot),
            other => panic!("expected Allow on attempt 2, got {other:?}"),
        }
        requests += 1;

        // Third attempt: requests=2 >= limit=2 → RevokeRpm. Counter stays at 2.
        let rpm = RpmView { requests, limit };
        match rate_limit_admission_decision(&rpm) {
            AdmissionDecision::RevokeRpm => {}
            other => panic!("expected RevokeRpm on attempt 3, got {other:?}"),
        }
        assert_eq!(requests, 2, "rejected attempt must NOT advance the counter");
    }

    /// Mirrors Go `TestRateLimitAdmission_SameChannelRetryCannotBypassRPM`:
    /// after a rejection, a same-channel retry on the same minute bucket is
    /// STILL rejected — Go does not consume a slot on RevokeRpm, so the counter
    /// stays put and the limit remains enforced.
    #[test]
    fn s25_same_channel_retry_cannot_bypass_rpm() {
        let limit: i64 = 1;

        // First attempt: admitted.
        let rpm = RpmView { requests: 0, limit };
        assert!(rate_limit_admission_decision(&rpm).is_allow());

        // Second attempt: rejected (counter at 1).
        let rpm = RpmView { requests: 1, limit };
        let decision = rate_limit_admission_decision(&rpm);
        assert!(decision.is_revoke_rpm());

        // Retry: STILL rejected (Go never touched the counter on rejection).
        let decision = rate_limit_admission_decision(&rpm);
        assert!(decision.is_revoke_rpm());
    }

    /// `consumed_slot` distinguishes the no-limit fast path (false) from the
    /// in-budget admit path (true). Mirrors Go's two distinct Allow returns.
    #[test]
    fn s25_consumed_slot_flag_distinguishes_no_limit_from_in_budget_admit() {
        // No-limit path: consumed_slot == false.
        let rpm = RpmView {
            requests: 999,
            limit: 0,
        };
        match rate_limit_admission_decision(&rpm) {
            AdmissionDecision::Allow { consumed_slot } => assert!(!consumed_slot),
            other => panic!("got {other:?}"),
        }

        // In-budget path: consumed_slot == true.
        let rpm = RpmView {
            requests: 0,
            limit: 10,
        };
        match rate_limit_admission_decision(&rpm) {
            AdmissionDecision::Allow { consumed_slot } => assert!(consumed_slot),
            other => panic!("got {other:?}"),
        }
    }

    /// Boundary: `requests == limit - 1` is the last admissible request; one
    /// more would tip into RevokeRpm.
    #[test]
    fn s25_boundary_requests_one_below_limit_is_last_admission() {
        let limit: i64 = 5;

        // requests = limit - 1: still admitted with slot consumed.
        let rpm = RpmView {
            requests: limit - 1,
            limit,
        };
        assert!(rate_limit_admission_decision(&rpm).is_allow());

        // requests = limit: rejected.
        let rpm = RpmView {
            requests: limit,
            limit,
        };
        assert!(rate_limit_admission_decision(&rpm).is_revoke_rpm());
    }

    /// `LOCAL_RPM_EXHAUSTED_MESSAGE` mirrors Go's
    /// `ErrLocalRPMExhausted = errors.New("local channel rpm exhausted")`
    /// verbatim.
    #[test]
    fn s25_local_rpm_exhausted_message_matches_go_literal() {
        assert_eq!(LOCAL_RPM_EXHAUSTED_MESSAGE, "local channel rpm exhausted");
    }

    // =========================================================================
    // S26 — withRateLimitTracking tests
    //   (mirror Go `rate_limit_tracking_test.go`)
    // =========================================================================

    /// Mirrors Go `TestRateLimitTracking_OnOutboundLlmResponse`'s
    /// "tracks tokens from response" sub-case: on a successful response with
    /// non-zero `TotalTokens`, the tracker adds those tokens.
    #[test]
    fn s26_success_with_nonzero_tokens_adds_tokens() {
        let outcome = AttemptOutcome {
            total_tokens: 150,
            succeeded: true,
            ..Default::default()
        };

        let delta = rate_limit_update(&outcome);

        assert_eq!(delta.tokens_added, 150);
        assert!(
            delta.cooldown_ms.is_none(),
            "success path must never set a cooldown"
        );
        assert!(!delta.is_empty());
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundLlmResponse`'s
    /// "handles nil usage" / "handles zero tokens" / "handles nil response"
    /// sub-cases: on a successful response with zero/missing tokens, the delta
    /// is empty (no token update, no cooldown).
    #[test]
    fn s26_success_with_zero_or_missing_tokens_is_noop() {
        for total_tokens in [0, -5] {
            let outcome = AttemptOutcome {
                total_tokens,
                succeeded: true,
                ..Default::default()
            };

            let delta = rate_limit_update(&outcome);

            assert_eq!(delta.tokens_added, 0, "total_tokens={total_tokens}");
            assert!(delta.cooldown_ms.is_none());
            assert!(delta.is_empty(), "total_tokens={total_tokens}");
        }
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundLlmResponse_MultipleChannels`
    /// and `_CombinedTokenOnly` semantics: token accounting accumulates across
    /// successive successful attempts (the wiring layer adds; the pure helper
    /// surfaces the per-attempt delta).
    #[test]
    fn s26_repeated_successes_accumulate_tokens_via_wiring() {
        let mut total = 0i64;
        for tokens in [100i64, 50] {
            let outcome = AttemptOutcome {
                total_tokens: tokens,
                succeeded: true,
                ..Default::default()
            };
            let delta = rate_limit_update(&outcome);
            assert_eq!(delta.tokens_added, tokens);
            total += delta.tokens_added;
        }
        assert_eq!(total, 150, "wiring layer sums the per-attempt deltas");
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundRawError_LocalRPMExhaustedIgnored`
    /// and `_QueueErrorIgnored`: a local admission rejection (queue error or
    /// RPM-exhausted) MUST NOT trigger a cooldown, even if the error otherwise
    /// carries a Retry-After header. The pure helper surfaces an empty delta.
    #[test]
    fn s26_local_admission_rejection_never_triggers_cooldown() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: true,
            is_http_429: true, // even if it looks like a 429
            has_retry_after: true,
            cooldown_ms: 30_000,
        };

        let delta = rate_limit_update(&outcome);

        assert!(
            delta.is_empty(),
            "local rejection must produce an empty delta"
        );
        assert!(delta.cooldown_ms.is_none());
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundRawError_429`: a 429 with a
    /// parseable Retry-After sets a cooldown for the channel.
    #[test]
    fn s26_http_429_with_retry_after_sets_cooldown() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: true,
            has_retry_after: true,
            cooldown_ms: 30_000,
        };

        let delta = rate_limit_update(&outcome);

        assert_eq!(delta.tokens_added, 0, "error path must not add tokens");
        assert_eq!(delta.cooldown_ms, Some(30_000));
        assert!(!delta.is_empty());
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundRawError_429WithoutRetryAfter`:
    /// a 429 *without* a Retry-After header does NOT set a cooldown.
    #[test]
    fn s26_http_429_without_retry_after_skips_cooldown() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: true,
            has_retry_after: false,
            cooldown_ms: 30_000, // irrelevant
        };

        let delta = rate_limit_update(&outcome);

        assert!(delta.is_empty());
        assert!(delta.cooldown_ms.is_none());
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundRawError_Not429`: a non-429
    /// error with a stray Retry-After header does NOT set a cooldown. Go only
    /// cools down on actual upstream 429s.
    #[test]
    fn s26_non_429_error_with_retry_after_skips_cooldown() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: false, // 500, 503, etc.
            has_retry_after: true,
            cooldown_ms: 30_000,
        };

        let delta = rate_limit_update(&outcome);

        assert!(delta.is_empty());
        assert!(delta.cooldown_ms.is_none());
    }

    /// Mirrors Go `TestRateLimitTracking_OnOutboundRawError_NoChannel` /
    /// `_NilChannel`: the pure helper is robust to "nothing to do" outcomes —
    /// an error path that is neither a 429 nor a local rejection produces an
    /// empty delta (the wiring layer would also short-circuit on nil channel
    /// before calling the helper, but the helper is independently safe).
    #[test]
    fn s26_non_429_non_local_error_without_retry_after_is_noop() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: false,
            has_retry_after: false,
            cooldown_ms: 0,
        };

        let delta = rate_limit_update(&outcome);
        assert!(delta.is_empty());
    }

    /// Success and cooldown paths are mutually exclusive: on success,
    /// `cooldown_ms` is always `None`; on error, `tokens_added` is always `0`.
    /// Mirrors how Go splits the two hooks (`OnOutboundLlmResponse` only adds
    /// tokens, `OnOutboundRawError` only sets cooldowns).
    #[test]
    fn s26_token_and_cooldown_paths_are_mutually_exclusive() {
        // Success with tokens → no cooldown.
        let success = rate_limit_update(&AttemptOutcome {
            total_tokens: 100,
            succeeded: true,
            is_http_429: true,
            has_retry_after: true,
            cooldown_ms: 999,
            ..Default::default()
        });
        assert_eq!(success.tokens_added, 100);
        assert!(success.cooldown_ms.is_none());

        // Error → no tokens.
        let error = rate_limit_update(&AttemptOutcome {
            total_tokens: 999, // ignored on error path
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: true,
            has_retry_after: true,
            cooldown_ms: 5_000,
        });
        assert_eq!(error.tokens_added, 0);
        assert_eq!(error.cooldown_ms, Some(5_000));
    }

    /// Boundary: a zero-ms cooldown is still a cooldown (Go would call
    /// `SetCooldown(channel.ID, now)`). The helper surfaces it as `Some(0)`,
    /// distinct from "no cooldown" (`None`).
    #[test]
    fn s26_zero_ms_cooldown_is_distinct_from_no_cooldown() {
        let outcome = AttemptOutcome {
            total_tokens: 0,
            succeeded: false,
            is_local_admission_rejection: false,
            is_http_429: true,
            has_retry_after: true,
            cooldown_ms: 0,
        };

        let delta = rate_limit_update(&outcome);

        assert_eq!(delta.cooldown_ms, Some(0));
        assert!(
            !delta.is_empty(),
            "Some(0) cooldown is still a non-empty delta"
        );
    }

    // =====================================================================
    // S29 — Process main-chain skeleton (Go `Process`)
    // =====================================================================

    /// S35 contract: the main-chain order is fixed as
    /// Auth -> Quota -> Select -> LoadBalance -> Pipeline -> Persist -> Emit.
    /// Mirrors Go `Process`'s middleware assembly order (inbound middlewares
    /// run in this exact sequence; outbound middlewares wrap the pipeline).
    #[test]
    fn s29_stage_sequence_is_fixed_main_chain_order() {
        let seq = stage_sequence();
        let expected = [
            OrchestratorStage::Auth,
            OrchestratorStage::Quota,
            OrchestratorStage::Select,
            OrchestratorStage::LoadBalance,
            OrchestratorStage::Pipeline,
            OrchestratorStage::Persist,
            OrchestratorStage::Emit,
        ];

        assert_eq!(
            *seq,
            expected[..],
            "Process main chain must match the S35 fixed order"
        );

        // stage_sequence() must agree with OrchestratorStage::ALL so callers
        // that already use the const see the same order.
        assert_eq!(stage_sequence(), &OrchestratorStage::ALL);
    }

    /// Every stage in the sequence has a plan, and the plans are emitted in
    /// stage-sequence order. Guards against a stage being added to the enum
    /// without a corresponding plan entry (or vice versa).
    #[test]
    fn s29_every_stage_has_a_plan_in_sequence_order() {
        let plans = StagePlan::table();

        assert_eq!(plans.len(), stage_sequence().len(), "one plan per stage");

        for (plan, stage) in plans.iter().zip(stage_sequence().iter()) {
            assert_eq!(&plan.stage, stage, "plan order must match stage_sequence()");
        }
    }

    /// `StagePlan::for_stage` round-trips every stage in the sequence.
    #[test]
    fn s29_for_stage_round_trips_every_stage() {
        for stage in stage_sequence().iter() {
            let plan = StagePlan::for_stage(*stage);
            assert_eq!(plan.map(|p| p.stage), Some(*stage));
        }
    }

    /// The plan's input/output tags must be non-empty (a stage with no inputs
    /// or no outputs is not part of the main chain — it would be a no-op).
    #[test]
    fn s29_every_plan_has_non_empty_inputs_and_outputs() {
        for plan in StagePlan::table() {
            assert!(
                !plan.inputs.is_empty(),
                "{:?} must declare at least one input",
                plan.stage
            );
            assert!(
                !plan.outputs.is_empty(),
                "{:?} must declare at least one output",
                plan.stage
            );
        }
    }

    /// Specific data-flow assertions mirroring Go `Process`:
    /// * `Select` produces `candidates` (consumed by `LoadBalance`).
    /// * `LoadBalance` produces `ordered_candidates` (consumed by `Pipeline`).
    /// * `Pipeline` produces `provider_response` (consumed by `Emit`).
    ///
    /// These are the load-bearing hand-offs in the Go flow — pinning them here
    /// guards against accidentally renaming the tags.
    #[test]
    fn s29_plan_outputs_match_go_handoffs() {
        // Use match (not .expect) to satisfy the workspace's deny-on-expect.
        let select = match StagePlan::for_stage(OrchestratorStage::Select) {
            Some(p) => p,
            None => panic!("Select must have a plan"),
        };
        assert!(
            select.outputs.contains(&"candidates"),
            "Select must output `candidates`"
        );

        let lb = match StagePlan::for_stage(OrchestratorStage::LoadBalance) {
            Some(p) => p,
            None => panic!("LoadBalance must have a plan"),
        };
        assert!(
            lb.outputs.contains(&"ordered_candidates"),
            "LoadBalance must output `ordered_candidates`"
        );
        assert!(
            lb.inputs.contains(&"candidates"),
            "LoadBalance must consume Select's `candidates`"
        );

        let pipeline = match StagePlan::for_stage(OrchestratorStage::Pipeline) {
            Some(p) => p,
            None => panic!("Pipeline must have a plan"),
        };
        assert!(
            pipeline.outputs.contains(&"provider_response"),
            "Pipeline must output `provider_response`"
        );

        let emit = match StagePlan::for_stage(OrchestratorStage::Emit) {
            Some(p) => p,
            None => panic!("Emit must have a plan"),
        };
        assert!(
            emit.inputs.contains(&"provider_response"),
            "Emit must consume Pipeline's `provider_response`"
        );
    }

    // =====================================================================
    // S30 — system-bypass locality
    // =====================================================================

    /// Stages that read internal data (auth / quota / select / persist) are
    /// granted the system bypass. Mirrors Go's `RunWithSystemBypass` discipline
    /// for those stages' internal reads.
    #[test]
    fn s30_internal_data_stages_get_bypass() {
        let internal_stages = [
            OrchestratorStage::Auth,
            OrchestratorStage::Quota,
            OrchestratorStage::Select,
            OrchestratorStage::Persist,
        ];

        for stage in internal_stages {
            assert_eq!(
                bypass_scope(stage),
                BypassScope::Internal,
                "{stage:?} reads internal data and must be granted the bypass"
            );
            assert!(bypass_scope(stage).is_internal());
        }
    }

    /// Stages that only touch the inbound request / upstream provider do NOT
    /// get the bypass. This is the S30 locality refinement — the bypass is
    /// scoped to internal-data reads, not blanket-applied to every stage.
    #[test]
    fn s30_non_internal_stages_get_no_bypass() {
        let external_stages = [
            OrchestratorStage::LoadBalance,
            OrchestratorStage::Pipeline,
            OrchestratorStage::Emit,
        ];

        for stage in external_stages {
            assert_eq!(
                bypass_scope(stage),
                BypassScope::None,
                "{stage:?} does not read internal data and must NOT be granted the bypass"
            );
            assert!(!bypass_scope(stage).is_internal());
        }
    }

    /// `bypass_scope` is total: every stage in the main chain has a defined
    /// scope (no panic / no uncovered variant).
    #[test]
    fn s30_bypass_scope_covers_every_stage() {
        for stage in stage_sequence().iter() {
            // Just assert it returns without panicking; the variant is total.
            let _ = bypass_scope(*stage);
        }
    }

    /// Pipeline explicitly does NOT get the bypass: the upstream provider call
    /// must stay user-scoped (the bypass must not grant upstream-side
    /// privileges). This is the most load-bearing S30 assertion — pin it
    /// separately.
    #[test]
    fn s30_pipeline_does_not_get_bypass() {
        assert_eq!(
            bypass_scope(OrchestratorStage::Pipeline),
            BypassScope::None,
            "Pipeline drives the upstream provider call and must stay user-scoped"
        );
    }

    // =====================================================================
    // S31/S32 — retry-policy derivation
    // =====================================================================

    /// Go `defaultRetryPolicy` parity: enabled, 3/2 retries, 1000ms delay,
    /// adaptive, empty-response detection off, both timeouts 0 (disabled),
    /// upstream-error passthrough.
    #[test]
    fn s32_default_policy_matches_go_default_retry_policy() {
        let default = ProcessRetryPolicy::default();

        assert!(default.enabled);
        assert_eq!(default.max_channel_retries, 3);
        assert_eq!(default.max_single_channel_retries, 2);
        assert_eq!(default.retry_delay_ms, 1000);
        assert_eq!(
            default.load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Adaptive
        );
        assert!(!default.empty_response_detection);
        assert_eq!(default.stream_first_event_timeout_seconds, 0);
        assert_eq!(default.non_stream_response_timeout_seconds, 0);
        assert_eq!(default.upstream_error_mode, UPSTREAM_ERROR_MODE_PASSTHROUGH);
    }

    /// Go `deriveLoadBalancerStrategy` table test parity (mirror
    /// `TestDeriveLoadBalancerStrategy`): nil/empty/`system_default` profile
    /// strategy falls back to the system strategy; a specific value overrides.
    #[test]
    fn s31_derive_strategy_mirrors_go_derive_load_balancer_strategy_table() {
        let system = ProcessRetryPolicy::default();
        // system strategy is Adaptive (default).

        // nil api-key / nil active profile / empty profile → system.
        assert_eq!(
            derive_retry_policy(system, &ApiKeyProfileOverride::none()).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Adaptive,
            "nil override keeps system strategy"
        );

        // empty strategy → system.
        let empty = ApiKeyProfileOverride {
            load_balance_strategy: Some(String::new()),
        };
        assert_eq!(
            derive_retry_policy(system, &empty).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Adaptive,
            "empty profile strategy keeps system strategy"
        );

        // system_default sentinel → system.
        let sentinel = ApiKeyProfileOverride {
            load_balance_strategy: Some(SYSTEM_DEFAULT_SENTINEL.to_string()),
        };
        assert_eq!(
            derive_retry_policy(system, &sentinel).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Adaptive,
            "system_default sentinel keeps system strategy"
        );

        // failover override wins.
        let failover = ApiKeyProfileOverride {
            load_balance_strategy: Some("failover".to_string()),
        };
        assert_eq!(
            derive_retry_policy(system, &failover).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Failover,
            "failover profile override wins"
        );

        // circuit-breaker override wins.
        let cb = ApiKeyProfileOverride {
            load_balance_strategy: Some("circuit-breaker".to_string()),
        };
        assert_eq!(
            derive_retry_policy(system, &cb).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::CircuitBreaker,
            "circuit-breaker profile override wins"
        );
    }

    /// Go `normalizeRetryPolicy`: `"weighted"` is rewritten to `failover`
    /// (deprecated strategy). The parsing delegates to
    /// `LoadBalancerStrategy::parse`, which applies the same normalization.
    #[test]
    fn s31_weighted_strategy_normalizes_to_failover() {
        let system = ProcessRetryPolicy::default();
        let weighted = ApiKeyProfileOverride {
            load_balance_strategy: Some("weighted".to_string()),
        };

        assert_eq!(
            derive_retry_policy(system, &weighted).load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Failover,
            "weighted override normalizes to failover (Go normalizeRetryPolicy)"
        );
    }

    /// The override only touches the load-balance strategy — every other field
    /// stays at the system value. Mirrors Go `deriveLoadBalancerStrategy`
    /// reading ONLY the `LoadBalanceStrategy` profile field.
    #[test]
    fn s31_override_only_touches_strategy_not_retry_counts_or_timeouts() {
        let system = ProcessRetryPolicy {
            enabled: true,
            max_channel_retries: 5,
            max_single_channel_retries: 4,
            retry_delay_ms: 2500,
            stream_first_event_timeout_seconds: 30,
            non_stream_response_timeout_seconds: 60,
            empty_response_detection: true,
            load_balancer_strategy: crate::load_balancer::LoadBalancerStrategy::Adaptive,
            upstream_error_mode: UPSTREAM_ERROR_MODE_HIDDEN,
        };

        let override_ = ApiKeyProfileOverride {
            load_balance_strategy: Some("failover".to_string()),
        };

        let derived = derive_retry_policy(system, &override_);

        // Strategy overridden.
        assert_eq!(
            derived.load_balancer_strategy,
            crate::load_balancer::LoadBalancerStrategy::Failover
        );

        // Every other field untouched.
        assert_eq!(derived.enabled, system.enabled);
        assert_eq!(derived.max_channel_retries, 5);
        assert_eq!(derived.max_single_channel_retries, 4);
        assert_eq!(derived.retry_delay_ms, 2500);
        assert_eq!(derived.stream_first_event_timeout_seconds, 30);
        assert_eq!(derived.non_stream_response_timeout_seconds, 60);
        assert!(derived.empty_response_detection);
        assert_eq!(derived.upstream_error_mode, UPSTREAM_ERROR_MODE_HIDDEN);
    }

    /// `overrides_strategy()` predicate mirrors Go's nil/empty/`system_default`
    /// short-circuit exactly.
    #[test]
    fn s31_overrides_strategy_predicate_matches_go_short_circuit() {
        assert!(
            !ApiKeyProfileOverride::none().overrides_strategy(),
            "None never overrides"
        );

        assert!(
            !ApiKeyProfileOverride {
                load_balance_strategy: Some(String::new()),
            }
            .overrides_strategy(),
            "empty string never overrides"
        );

        assert!(
            !ApiKeyProfileOverride {
                load_balance_strategy: Some(SYSTEM_DEFAULT_SENTINEL.to_string()),
            }
            .overrides_strategy(),
            "system_default sentinel never overrides"
        );

        assert!(
            ApiKeyProfileOverride {
                load_balance_strategy: Some("adaptive".to_string()),
            }
            .overrides_strategy(),
            "any other non-empty value overrides"
        );
    }

    /// `to_lb_policy` round-trips the LB-facing fields. Mirrors how Go threads
    /// the same `biz.RetryPolicy` into both the pipeline options and the
    /// `LoadBalancer`.
    #[test]
    fn s32_to_lb_policy_round_trips_lb_facing_fields() {
        let system = ProcessRetryPolicy {
            enabled: true,
            max_channel_retries: 4,
            max_single_channel_retries: 3,
            retry_delay_ms: 500,
            stream_first_event_timeout_seconds: 0,
            non_stream_response_timeout_seconds: 0,
            empty_response_detection: false,
            load_balancer_strategy: crate::load_balancer::LoadBalancerStrategy::Failover,
            upstream_error_mode: UPSTREAM_ERROR_MODE_PASSTHROUGH,
        };

        let lb = system.to_lb_policy();

        assert!(lb.enabled);
        assert_eq!(lb.max_channel_retries, 4);
        assert_eq!(lb.max_single_channel_retries, 3);
        assert_eq!(lb.retry_delay_ms, 500);
        assert_eq!(
            lb.strategy,
            crate::load_balancer::LoadBalancerStrategy::Failover
        );
    }

    /// `attach_empty_response_detection` mirrors Go's gate: only when retry is
    /// enabled AND the flag is set.
    #[test]
    fn s32_attach_empty_response_detection_gates_on_enabled_and_flag() {
        // enabled + flag → attach.
        let on = ProcessRetryPolicy {
            enabled: true,
            empty_response_detection: true,
            ..ProcessRetryPolicy::default()
        };
        assert!(on.attach_empty_response_detection());

        // enabled + flag off → do not attach.
        let off = ProcessRetryPolicy {
            enabled: true,
            empty_response_detection: false,
            ..ProcessRetryPolicy::default()
        };
        assert!(!off.attach_empty_response_detection());

        // disabled + flag on → do not attach (Go wraps in `if Enabled`).
        let disabled = ProcessRetryPolicy {
            enabled: false,
            empty_response_detection: true,
            ..ProcessRetryPolicy::default()
        };
        assert!(!disabled.attach_empty_response_detection());
    }

    /// `attach_response_timeouts` mirrors Go's gate: only when retry is enabled
    /// AND at least one timeout is non-zero.
    #[test]
    fn s32_attach_response_timeouts_gates_on_enabled_and_non_zero_timeout() {
        // enabled + non-zero stream timeout → attach.
        let stream = ProcessRetryPolicy {
            enabled: true,
            stream_first_event_timeout_seconds: 30,
            non_stream_response_timeout_seconds: 0,
            ..ProcessRetryPolicy::default()
        };
        assert!(stream.attach_response_timeouts());

        // enabled + non-zero non-stream timeout → attach.
        let non_stream = ProcessRetryPolicy {
            enabled: true,
            stream_first_event_timeout_seconds: 0,
            non_stream_response_timeout_seconds: 60,
            ..ProcessRetryPolicy::default()
        };
        assert!(non_stream.attach_response_timeouts());

        // enabled + both zero → do not attach (no-op).
        let zero = ProcessRetryPolicy::default();
        assert!(!zero.attach_response_timeouts());

        // disabled + non-zero → do not attach (Go wraps in `if Enabled`).
        let disabled = ProcessRetryPolicy {
            enabled: false,
            stream_first_event_timeout_seconds: 30,
            ..ProcessRetryPolicy::default()
        };
        assert!(!disabled.attach_response_timeouts());
    }

    // -------------------------------------------------------------------
    // RUST-P9-006 S33 — failure_persistence_plan (Go orchestrator.go:299-328)
    // -------------------------------------------------------------------

    /// The detached-context timeout the Go orchestrator hands to failure
    /// persistence is exactly 10 seconds (Go `time.Second*10`). Surfaced in
    /// milliseconds so the wiring layer can feed it straight into
    /// `tokio::time::timeout`. Pinned at 10_000 to match
    /// `conduit/internal/server/orchestrator/orchestrator.go:301` and
    /// `request_execution.go:132`/`:217`.
    #[test]
    fn s33_detached_timeout_is_exactly_ten_seconds() {
        assert_eq!(FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS, 10_000);
    }

    /// The terminal status the orchestrator-side error branch writes is always
    /// `Failed` — Go's `UpdateRequestStatusFromError` may further distinguish
    /// canceled, but the orchestrator-side contract is "leaving Running as a
    /// terminal failure". See [`FAILURE_PERSISTENCE_TERMINAL_STATUS`] doc.
    #[test]
    fn s33_terminal_status_is_failed() {
        assert_eq!(FAILURE_PERSISTENCE_TERMINAL_STATUS, RequestStatus::Failed);
    }

    /// Go orchestrator.go error branch:
    ///   persistCtx, cancel := xcontext.DetachWithTimeout(ctx, time.Second*10)
    ///   if requestExec := outbound.GetRequestExecution(); requestExec != nil {
    ///       UpdateRequestExecutionStatusFromError(persistCtx, requestExec.ID, err)
    ///   }
    ///   if request := outbound.GetRequest(); request != nil {
    ///       UpdateRequestStatusFromError(persistCtx, request.ID, err)
    ///   }
    /// The plan must carry the bubbled-up error message + both ids + the 10s
    /// detached timeout, with Failed as the terminal status.
    #[test]
    fn s33_plan_carries_error_message_ids_and_detached_timeout()
    -> Result<(), Box<dyn std::error::Error>> {
        let err = ConduitError::internal("upstream 502");
        let plan =
            failure_persistence_plan(&err, Some("req-1".to_string()), Some("exec-1".to_string()));

        assert_eq!(plan.final_request_status, RequestStatus::Failed);
        // The error message is the display string of the bubbled-up ConduitError.
        assert!(
            plan.error_message.contains("upstream 502"),
            "error_message should carry the bubbled-up error text, got: {}",
            plan.error_message
        );
        assert_eq!(plan.detached_timeout_ms, 10_000);
        assert_eq!(plan.request_id.as_deref(), Some("req-1"));
        assert_eq!(plan.execution_id.as_deref(), Some("exec-1"));
        assert!(plan.persists_request());
        assert!(plan.persists_execution());
        Ok(())
    }

    /// Go guards both persistence calls with nil-checks on
    /// `outbound.GetRequest()` / `outbound.GetRequestExecution()`. When the
    /// pipeline failed before `persistRequestExecution` ran (or even before
    /// `persistRequest`), the ids are absent and the plan must reflect that —
    /// the wiring layer should then skip the corresponding recorder call.
    #[test]
    fn s33_plan_with_no_ids_reports_no_persistence() {
        let err = ConduitError::internal("boom");
        let plan = failure_persistence_plan(&err, None, None);

        assert_eq!(plan.final_request_status, RequestStatus::Failed);
        assert!(
            !plan.persists_request(),
            "no request id => no request write"
        );
        assert!(
            !plan.persists_execution(),
            "no execution id => no execution write"
        );
    }

    /// Go allows the execution to exist while the request row does not (rare,
    /// but the nil-checks are independent). The plan must surface each id
    /// independently so the wiring can issue just the recorder call that has a
    /// target.
    #[test]
    fn s33_plan_request_only_when_execution_absent() {
        let err = ConduitError::internal("late failure");
        let plan = failure_persistence_plan(&err, Some("req-7".to_string()), None);

        assert!(plan.persists_request());
        assert!(!plan.persists_execution());
        assert_eq!(plan.request_id.as_deref(), Some("req-7"));
        assert!(plan.execution_id.is_none());
    }

    /// Go's `ExtractErrorMessage` pulls `error.message` / `errors.0.message` /
    /// `errors.message` out of an `*httpclient.Error` body, falling back to
    /// `err.Error()`. S33's plan surfaces the raw display string of the
    /// bubbled-up error; the structured HTTP-body extraction is the recorder's
    /// job. The test pins that the plan carries the full diagnostic string
    /// verbatim (no truncation, no redaction).
    #[test]
    fn s33_error_message_is_the_verbatim_error_display() {
        let msg = "connection reset by peer: upstream channel timeout after 30s";
        let err = ConduitError::internal(msg);
        let plan = failure_persistence_plan(&err, None, None);

        assert_eq!(plan.error_message, err.to_string());
        assert!(
            plan.error_message.contains(msg),
            "plan must preserve the diagnostic text for the recorder, got: {}",
            plan.error_message
        );
    }

    // -------------------------------------------------------------------
    // RUST-P9-006 S34 — stream_final_plan (Go outbound.go:100-212)
    // -------------------------------------------------------------------

    /// The detached timeout on the stream-final path matches the one on the
    /// failure path: 10s. Mirrors `xcontext.DetachWithTimeout(ctx,
    /// 10*time.Second)` at outbound.go:130/162/184/246.
    #[test]
    fn s34_detached_timeout_is_exactly_ten_seconds() {
        assert_eq!(STREAM_FINAL_DETACHED_TIMEOUT_MS, 10_000);
    }

    /// The "no terminal event" sentinel message must match the Go literal
    /// verbatim — `errors.New("stream ended without terminal event or
    /// completed response")` at outbound.go:170 and :187.
    #[test]
    fn s34_no_terminal_event_message_matches_go_literal() {
        assert_eq!(
            STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE,
            "stream ended without terminal event or completed response"
        );
    }

    /// Go branch table for `OutboundPersistentStream.Close`:
    ///   completed_normally=true => Succeeded, write chunks + usage.
    /// Pins the success row of the S34 truth table.
    #[test]
    fn s34_completed_normally_writes_chunks_and_usage_as_succeeded() {
        // Client disconnect is ignored when the terminal event arrived — Go's
        // explicit comment at outbound.go:114-117 handles the "client
        // disconnects immediately after the last chunk" case as a success.
        for client_disconnected in [false, true] {
            let plan = stream_final_plan(true, client_disconnected);
            assert_eq!(
                plan.final_status, STREAM_FINAL_COMPLETED_STATUS,
                "completed stream must be Succeeded regardless of client_disconnected (got client_disconnected={})",
                client_disconnected
            );
            assert_eq!(plan.final_status, RequestStatus::Succeeded);
            assert!(plan.write_chunks, "completed stream must persist chunks");
            assert!(plan.write_usage, "completed stream must persist usage");
            assert!(
                plan.error_message.is_none(),
                "completed stream records no error"
            );
            assert_eq!(plan.detached_timeout_ms, 10_000);
            assert!(plan.is_completed());
            assert!(!plan.is_canceled());
        }
    }

    /// Go "incomplete_stream_with_error" branch (outbound.go:160-180): when
    /// the stream ends without a terminal event AND the context was canceled
    /// (or the stream surfaced context.Canceled), the recorder writes a
    /// Canceled status and does NOT persist chunks/usage. Pins the
    /// client-disconnect row of the S34 truth table.
    #[test]
    fn s34_client_disconnect_yields_canceled_without_chunks_or_usage() {
        let plan = stream_final_plan(false, true);

        assert_eq!(plan.final_status, RequestStatus::Cancelled);
        assert_eq!(plan.final_status, STREAM_FINAL_CANCELED_STATUS);
        assert!(
            !plan.write_chunks,
            "client disconnect must NOT persist partial chunks as a completion"
        );
        assert!(
            !plan.write_usage,
            "client disconnect must NOT write a usage log for an incomplete response"
        );
        assert!(
            plan.error_message.is_some(),
            "canceled plan carries an error message for the recorder"
        );
        assert_eq!(plan.detached_timeout_ms, 10_000);
        assert!(plan.is_canceled());
        assert!(!plan.is_completed());
    }

    /// Go "incomplete_stream_without_terminal_event" branch (outbound.go:182-
    /// 195): the stream ended without [DONE] and without a client cancel —
    /// the recorder writes Failed with the Go sentinel message and does NOT
    /// persist chunks/usage. Pins the abnormal-end row of the S34 truth table.
    #[test]
    fn s34_no_terminal_event_no_disconnect_yields_failed_with_sentinel() {
        let plan = stream_final_plan(false, false);

        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert_eq!(plan.final_status, STREAM_FINAL_FAILED_STATUS);
        assert!(!plan.write_chunks);
        assert!(!plan.write_usage);
        assert_eq!(
            plan.error_message.as_deref(),
            Some(STREAM_FINAL_NO_TERMINAL_EVENT_MESSAGE),
            "no-terminal-event plan must carry the Go sentinel verbatim"
        );
        assert_eq!(plan.detached_timeout_ms, 10_000);
        assert!(!plan.is_completed());
        assert!(!plan.is_canceled());
    }

    /// The truth table is fully covered by three input combinations
    /// (completed+disconnect is collapsed onto the completed row). This is a
    /// meta-test that pins the exhaustive branch table in one place so a
    /// future refactor cannot silently change one cell.
    #[test]
    fn s34_branch_table_is_exhaustive_and_matches_go_close_semantics() {
        let cases = [
            // (completed, disconnected) -> (status, write_chunks, write_usage, has_error_msg)
            (true, false, RequestStatus::Succeeded, true, true, false),
            (true, true, RequestStatus::Succeeded, true, true, false),
            (false, true, RequestStatus::Cancelled, false, false, true),
            (false, false, RequestStatus::Failed, false, false, true),
        ];

        for (completed, disconnected, status, chunks, usage, has_err) in cases {
            let plan = stream_final_plan(completed, disconnected);
            assert_eq!(
                plan.final_status, status,
                "completed={}, disconnected={} should map to {:?}",
                completed, disconnected, status
            );
            assert_eq!(
                plan.write_chunks, chunks,
                "write_chunks mismatch for completed={}, disconnected={}",
                completed, disconnected
            );
            assert_eq!(
                plan.write_usage, usage,
                "write_usage mismatch for completed={}, disconnected={}",
                completed, disconnected
            );
            assert_eq!(
                plan.error_message.is_some(),
                has_err,
                "error_message presence mismatch for completed={}, disconnected={}",
                completed,
                disconnected
            );
            assert_eq!(
                plan.detached_timeout_ms, 10_000,
                "detached timeout is constant across all branches"
            );
        }
    }

    /// Live-preview transparency (Go `live_streaming.go`): the live-preview
    /// path forwards every chunk to the client unchanged. S34's plan does not
    /// model the live-preview write itself (that is the
    /// `livePreviewMiddleware`'s job, not `OutboundPersistentStream.Close`'s),
    /// but the success branch must NOT alter the chunks the client already
    /// saw — `write_chunks` persists the SAME chunks the live preview
    /// forwarded. This test pins that contract by asserting the success plan
    /// asks for a chunk write (the live-preview buffer and the persistent
    /// buffer receive identical events in Go).
    #[test]
    fn s34_success_plan_preserves_live_preview_transparency_contract() {
        let plan = stream_final_plan(true, false);
        // The success branch persists chunks; the live-preview path does not
        // mutate them. The wiring layer is responsible for feeding the same
        // chunk slice to both paths (mirrors Go's `liveRequestStream.Next`
        // Append + Current split).
        assert!(plan.write_chunks);
        assert!(plan.is_completed());
    }
}

/// P-17: pick the plaintext credential for a candidate at request-execution
/// time, when the request trace id is finally available.
///
/// The candidate carries the *full* enabled-key set (`enabled_credentials`),
/// deferred from snapshot-build time — where no request trace exists yet, so
/// the old `active_credential` was always the deterministic `enabled[0]`,
/// concentrating all load + quota on the first key. Here the trace id (when
/// present) drives rendezvous (HRW) selection so N keys spread evenly and a
/// given trace stays sticky to one key; without a trace we fall back to the
/// snapshot's `active_credential` (still `enabled[0]`, matching the prior
/// behavior for trace-less calls).
///
/// OAuth/Azure/GCP channels have an empty `enabled_credentials` (their auth
/// materializes in the transformer layer), so they fall through to
/// `active_credential` (which is `None` for them) unchanged.
///
/// ⚠ Returns a plaintext secret: in-memory only — never log it.
fn select_trace_sticky_credential(
    enabled_credentials: &[String],
    active_credential: Option<&str>,
    trace_id: Option<&str>,
) -> Option<String> {
    match (enabled_credentials.len(), trace_id) {
        // Multi-key + trace: rendezvous-select so load spreads across keys and
        // the same trace stays sticky (Go `TraceStickyKeyProvider`).
        (n, Some(trace)) if n > 1 => {
            conduit_services::rendezvous_select(enabled_credentials, trace)
                .map(str::to_string)
                .or_else(|| active_credential.map(str::to_string))
        }
        // Single key or no trace: the snapshot's deterministic pick is correct
        // (single key = that key; no trace = Go's non-sticky path).
        _ => active_credential.map(str::to_string),
    }
}

fn select_healthy_credential(
    enabled_credentials: &[String],
    active_credential: Option<&str>,
    trace_id: Option<&str>,
    channel_id: &str,
    actual_model: &str,
    statuses: &BTreeMap<RouteHealthTarget, RouteHealthStatus>,
) -> Option<Option<String>> {
    let preferred =
        select_trace_sticky_credential(enabled_credentials, active_credential, trace_id);
    let mut choices = Vec::new();
    if preferred.is_some() {
        choices.push(preferred);
    }
    for credential in enabled_credentials {
        let choice = Some(credential.clone());
        if !choices.contains(&choice) {
            choices.push(choice);
        }
    }
    if choices.is_empty() {
        choices.push(None);
    }
    choices.into_iter().find(|credential| {
        let target = RouteHealthTarget {
            channel_id: channel_id.to_string(),
            actual_model: actual_model.to_string(),
            credential_identity: credential
                .as_deref()
                .map(conduit_services::credential_fingerprint),
        };
        statuses.get(&target).copied() != Some(RouteHealthStatus::Unhealthy)
    })
}

#[cfg(test)]
mod route_health_credential_tests {
    use super::*;

    #[test]
    fn unhealthy_preferred_key_falls_back_without_exposing_plaintext() {
        let bad = "provider-key-bad".to_string();
        let good = "provider-key-good".to_string();
        let target = RouteHealthTarget {
            channel_id: "7".into(),
            actual_model: "same-model".into(),
            credential_identity: Some(conduit_services::credential_fingerprint(&bad)),
        };
        let statuses = BTreeMap::from([(target, RouteHealthStatus::Unhealthy)]);

        let selected = select_healthy_credential(
            &[bad.clone(), good.clone()],
            Some(&bad),
            None,
            "7",
            "same-model",
            &statuses,
        );

        assert_eq!(selected, Some(Some(good)));
        assert!(statuses.keys().all(|target| {
            target
                .credential_identity
                .as_deref()
                .is_none_or(|identity| !identity.contains("provider-key"))
        }));
    }

    #[test]
    fn all_unhealthy_keys_exclude_the_target() {
        let keys = vec!["bad-a".to_string(), "bad-b".to_string()];
        let statuses = keys
            .iter()
            .map(|key| {
                (
                    RouteHealthTarget {
                        channel_id: "7".into(),
                        actual_model: "same-model".into(),
                        credential_identity: Some(conduit_services::credential_fingerprint(key)),
                    },
                    RouteHealthStatus::Unhealthy,
                )
            })
            .collect();

        assert_eq!(
            select_healthy_credential(&keys, Some(&keys[0]), None, "7", "same-model", &statuses,),
            None
        );
    }
}

#[cfg(test)]
mod route_affinity_tests {
    use super::*;
    use conduit_core::objects::channel_settings::ChannelEndpoint;
    use conduit_services::channel_service::{ChannelModelEntry, ModelSource};

    fn candidate(
        channel_id: &str,
        actual_model: &str,
        api_format: &str,
        credentials: &[&str],
    ) -> ChannelModelsCandidate {
        ChannelModelsCandidate {
            channel_id: channel_id.into(),
            channel_name: format!("channel-{channel_id}"),
            ordering_weight: 0,
            priority: 0,
            models: vec![ChannelModelEntry {
                request_model: "public-model".into(),
                actual_model: actual_model.into(),
                source: ModelSource::Direct,
            }],
            endpoint: ChannelEndpoint {
                api_format: api_format.into(),
                ..Default::default()
            },
            api_format: api_format.into(),
            channel_type: "openai".into(),
            policies: Default::default(),
            credential_key_identity: String::new(),
            tags: Vec::new(),
            base_url: None,
            active_credential: credentials.first().map(|value| (*value).to_string()),
            enabled_credentials: credentials
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            settings: None,
            theoretical_cost_accounting: None,
            cost_efficiency_score: 0,
        }
    }

    fn hint(
        key_class: &str,
        channel_id: &str,
        actual_model: &str,
        api_format: &str,
        credential: Option<&str>,
    ) -> RouteAffinityHint {
        RouteAffinityHint {
            key_class: key_class.into(),
            channel_id: channel_id.into(),
            upstream_model_id: actual_model.into(),
            upstream_api_format: api_format.into(),
            credential_identity: credential.map(conduit_services::credential_fingerprint),
        }
    }

    #[test]
    fn previous_response_affinity_has_priority_over_prompt_cache_affinity() {
        let resolved = vec![
            candidate("1", "upstream", "openai/responses", &["key-a"]),
            candidate("2", "upstream", "openai/responses", &["key-b"]),
        ];
        let hints = vec![
            hint(
                conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                "2",
                "upstream",
                "openai/responses",
                Some("key-b"),
            ),
            hint(
                conduit_db::KEY_CLASS_PROMPT_CACHE_KEY,
                "1",
                "upstream",
                "openai/responses",
                Some("key-a"),
            ),
        ];

        let selected = resolve_route_affinity(&hints, &resolved, &[0, 1], &BTreeMap::new());

        assert_eq!(selected.as_ref().map(|selected| selected.index), Some(1));
        assert_eq!(
            selected
                .as_ref()
                .map(|selected| selected.key_class.as_str()),
            Some(conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID)
        );
        assert_eq!(
            selected
                .as_ref()
                .and_then(|selected| selected.credential.as_deref()),
            Some("key-b")
        );
    }

    #[test]
    fn stale_model_format_or_disabled_credential_cannot_resurrect_route() {
        let resolved = vec![candidate(
            "2",
            "new-upstream",
            "openai/chat_completions",
            &["enabled-key"],
        )];
        for stale in [
            hint(
                conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                "2",
                "old-upstream",
                "openai/chat_completions",
                Some("enabled-key"),
            ),
            hint(
                conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                "2",
                "new-upstream",
                "openai/responses",
                Some("enabled-key"),
            ),
            hint(
                conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                "2",
                "new-upstream",
                "openai/chat_completions",
                Some("disabled-key"),
            ),
        ] {
            assert!(resolve_route_affinity(&[stale], &resolved, &[0], &BTreeMap::new()).is_none());
        }
    }

    #[test]
    fn unhealthy_continuity_credential_falls_through_to_prompt_cache_hint() {
        let resolved = vec![
            candidate("1", "upstream", "openai/responses", &["key-a"]),
            candidate("2", "upstream", "openai/responses", &["key-b"]),
        ];
        let hints = vec![
            hint(
                conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                "2",
                "upstream",
                "openai/responses",
                Some("key-b"),
            ),
            hint(
                conduit_db::KEY_CLASS_PROMPT_CACHE_KEY,
                "1",
                "upstream",
                "openai/responses",
                Some("key-a"),
            ),
        ];
        let unhealthy = BTreeMap::from([(
            RouteHealthTarget {
                channel_id: "2".into(),
                actual_model: "upstream".into(),
                credential_identity: Some(conduit_services::credential_fingerprint("key-b")),
            },
            RouteHealthStatus::Unhealthy,
        )]);

        let selected = resolve_route_affinity(&hints, &resolved, &[0, 1], &unhealthy);

        assert_eq!(selected.as_ref().map(|selected| selected.index), Some(0));
        assert_eq!(
            selected
                .as_ref()
                .map(|selected| selected.key_class.as_str()),
            Some(conduit_db::KEY_CLASS_PROMPT_CACHE_KEY)
        );
    }
}

/// Build channel-specific config entries from the resolved candidate.
fn build_channel_config(
    candidate: &crate::candidates::ChannelModelsCandidate,
) -> std::collections::BTreeMap<String, String> {
    let mut config = std::collections::BTreeMap::new();
    let Some(settings) = candidate.settings.as_ref() else {
        return config;
    };

    if let Some(enabled) = settings.pass_through_body {
        config.insert("pass_through_enabled".to_string(), enabled.to_string());
    }
    if let Some(rate_limit) = settings.rate_limit.as_ref() {
        if let Some(max_concurrent) = rate_limit.max_concurrent {
            config.insert(
                "channel_max_concurrent".to_string(),
                max_concurrent.to_string(),
            );
        }
        if let Some(rpm) = rate_limit.rpm {
            config.insert("channel_rpm_limit".to_string(), rpm.to_string());
        }
    }
    if !settings.body_override_operations.is_empty()
        && let Ok(json) = serde_json::to_string(&settings.body_override_operations)
    {
        config.insert("channel_body_overrides".to_string(), json);
    }
    if !settings.header_override_operations.is_empty()
        && let Ok(json) = serde_json::to_string(&settings.header_override_operations)
    {
        config.insert("channel_header_overrides".to_string(), json);
    }
    if !settings.override_headers.is_empty()
        && let Ok(json) = serde_json::to_string(&settings.override_headers)
    {
        config.insert("channel_override_headers".to_string(), json);
    }
    if !settings.override_parameters.is_empty() {
        config.insert(
            "channel_override_parameters".to_string(),
            settings.override_parameters.clone(),
        );
    }
    if let Some(enabled) = settings.pass_through_user_agent {
        config.insert("pass_through_user_agent".to_string(), enabled.to_string());
    }
    if let Some(proxy) = settings.proxy.as_ref()
        && let Ok(json) = serde_json::to_string(proxy)
    {
        config.insert("channel_proxy".to_string(), json);
    }

    config
}

#[cfg(test)]
mod channel_config_tests {
    use conduit_core::objects::channel_settings::{ChannelRateLimit, ChannelSettings};
    use conduit_core::objects::overrides::OverrideOperation;

    use super::build_channel_config;
    use crate::candidates::ChannelModelsCandidate;

    #[test]
    fn channel_settings_become_pipeline_metadata() {
        let candidate = ChannelModelsCandidate {
            channel_id: "ch-1".to_string(),
            channel_name: "OpenAI".to_string(),
            ordering_weight: 0,
            priority: 0,
            models: Vec::new(),
            endpoint: conduit_core::objects::channel_settings::ChannelEndpoint {
                api_format: "openai/chat_completions".to_string(),
                ..Default::default()
            },
            api_format: "openai/chat_completions".to_string(),
            channel_type: "openai".to_string(),
            policies: Default::default(),
            credential_key_identity: String::new(),
            tags: Vec::new(),
            base_url: None,
            active_credential: None,
            enabled_credentials: Vec::new(),
            settings: Some(ChannelSettings {
                pass_through_body: Some(true),
                proxy: Some(serde_json::json!({
                    "type": "URL",
                    "url": "http://proxy.example:8080"
                })),
                rate_limit: Some(ChannelRateLimit {
                    rpm: Some(120),
                    max_concurrent: Some(4),
                    ..Default::default()
                }),
                body_override_operations: vec![OverrideOperation {
                    op: "set".to_string(),
                    path: "temperature".to_string(),
                    value: "0.2".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            theoretical_cost_accounting: None,
            cost_efficiency_score: 0,
        };

        let config = build_channel_config(&candidate);

        assert_eq!(
            config.get("pass_through_enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            config.get("channel_max_concurrent").map(String::as_str),
            Some("4")
        );
        assert_eq!(
            config.get("channel_rpm_limit").map(String::as_str),
            Some("120")
        );
        let proxy: serde_json::Value = config
            .get("channel_proxy")
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default();
        assert_eq!(proxy["type"], "URL");
        assert_eq!(proxy["url"], "http://proxy.example:8080");
        let overrides: Vec<OverrideOperation> = match config.get("channel_body_overrides") {
            Some(json_str) => match serde_json::from_str(json_str) {
                Ok(v) => v,
                Err(e) => panic!("Test failure: valid override JSON, got {}", e),
            },
            None => panic!("Test failure: override metadata missing"),
        };
        let candidate_ops = match candidate.settings {
            Some(ref s) => &s.body_override_operations,
            None => panic!("Test failure: candidate.settings missing"),
        };
        assert_eq!(&overrides, candidate_ops);
    }
}

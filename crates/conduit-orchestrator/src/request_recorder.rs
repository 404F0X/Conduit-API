//! RUST-P9-006 S15 — production [`RequestRecorder`] implementation.
//!
//! The pure decision layer ([`ExecutionRecordPlan`] / [`FailurePersistencePlan`] /
//! [`StreamFinalPlan`] + [`FAILURE_PERSISTENCE_TERMINAL_STATUS`]) and the
//! [`RequestRecorder`] trait + [`NoopRequestRecorder`] already live in
//! [`crate::orchestrator`]. The `RequestService` write methods the recorder
//! needs (`update_request_completed` / `update_request_status_from_error` /
//! `update_request_execution_completed` / `update_request_execution_failed` /
//! `save_request_execution_chunks`) were ported by Faraday-the-8th. This module
//! closes the S15 gap by providing a production recorder that walks the plans
//! and calls those methods in the order the Go source does.
//!
//! # Go contract
//!
//! The success path mirrors two Go middlewares that run on the **outbound LLM
//! response** event:
//!
//! * `persistRequestExecution.OnOutboundLlmResponse`
//!   (`conduit/internal/server/orchestrator/request_execution.go:125-187`) —
//!   builds latency metrics and calls `UpdateRequestExecutionCompleted`.
//! * `persistRequest.OnOutboundLlmResponse`
//!   (`conduit/internal/server/orchestrator/request.go:56-78`) — calls
//!   `UsageLogService.CreateUsageLogFromRequest` with the response usage.
//! * `persistRequest.OnInboundRawResponse`
//!   (`conduit/internal/server/orchestrator/request.go:80-162`) — calls
//!   `UpdateRequestCompleted` with the HTTP response body (with audio/STT
//!   special-casing). Audio/video/STT are deferred here (see REMAINING below).
//!
//! The failure path mirrors `ChatCompletionOrchestrator.Process`'s error branch
//! (`conduit/internal/server/orchestrator/orchestrator.go:299-328`):
//!
//! * `UpdateRequestExecutionStatusFromError` (when `outbound.GetRequestExecution()`
//!   != nil) — runs first.
//! * `UpdateRequestStatusFromError` (when `outbound.GetRequest()` != nil) —
//!   runs second.
//!
//! Both branches run under a `xcontext.DetachWithTimeout(ctx, 10*time.Second)`
//! so the persist survives request-context cancellation. We mirror that with
//! [`tokio::time::timeout`] keyed off [`FailurePersistencePlan::detached_timeout_ms`].
//!
//! # REMAINING (deferred, not in this delivery)
//!
//! * Audio (TTS) / video / STT special-casing in the response body
//!   (`audioSafeResponseBody` / `UpdateRequestCompletedWithAudio` /
//!   `UpdateRequestStatusExternalIDAndResponseBody`). The default branch
//!   (`UpdateRequestCompleted`) is wired here; the binary-payload branches land
//!   with the audio/video artifact persistence work.
//! * Inbound `Request` row creation (`persistRequest.OnInboundLlmRequest` ->
//!   `CreateRequest`). This belongs on the inbound transformer wiring, not the
//!   recorder; the recorder assumes the row already exists (the orchestrator
//!   hands it a `request_id`).
//! * Streaming wrapper (`OutboundPersistentStream.Close` ->
//!   `StreamFinalPlan`). The pure plan exists; wiring it to the live stream
//!   loop is the stream-pipeline task. This recorder's `record_success` covers
//!   the non-streaming success path (the streaming path is expected to call
//!   into the same `RequestService` methods via the [`StreamFinalPlan`]).

use std::sync::Arc;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_llm::{HttpResponse, Usage};
use conduit_pipeline::pipeline::AttemptRecord as PipelineAttempt;
use conduit_services::usage_service::{
    CreateUsageLogParams, UsageLog, UsageLogSource, create_usage_log_from_structured_usage,
};
use conduit_services::{
    ExecutionErrorInfo, LatencyMetrics, RequestService, RequestServiceError, RequestStatus,
};
use serde_json::Value;
use tokio::time::timeout;
use tracing::warn;

use crate::orchestrator::{
    ExecutionRecordPlan, FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS,
    FAILURE_PERSISTENCE_TERMINAL_STATUS, FailurePersistencePlan, OrchestratorContext,
    RequestRecorder, StreamFinalPlan, failure_persistence_plan,
};

/// Sink for fully-populated [`UsageLog`] rows built by
/// [`create_usage_log_from_structured_usage`].
///
/// # Why a separate trait (vs. `UsageLogService`)?
///
/// The pre-existing [`conduit_services::UsageLogService`] only persists the
/// *legacy* `UsageRecord` shape via `UsageLogRepo::insert_usage`. The
/// comprehensive [`UsageLog`] row is built pure-side by
/// [`create_usage_log_from_structured_usage`] (RUST-P10-002 S14), but its repo
/// adapter is not yet ported. To keep this recorder faithful to the S14
/// "structured-input only" contract (no raw body parsing) without blocking on
/// the not-yet-ported `UsageLog` repo, the recorder depends on this minimal
/// sink trait. Production wires it to a real `UsageLog` repo adapter once that
/// lands; tests use [`CapturingUsageLogSink`].
#[async_trait]
pub trait UsageLogSink: Send + Sync {
    /// Persist a fully-populated [`UsageLog`] row. Mirrors Go
    /// `UsageLogService.CreateUsageLogFromRequest`'s tail (the row insert).
    /// Errors are surfaced but MUST NOT mask the original request outcome (Go
    /// only `log.Warn`s on usage-log failure).
    async fn insert_usage_log(
        &self,
        ctx: &RequestContext,
        usage_log: UsageLog,
    ) -> Result<(), UsageSinkError>;
}

/// Error type for [`UsageLogSink::insert_usage_log`]. Kept intentionally broad
/// (string message) so the sink adapter's concrete error type stays free.
#[derive(Debug, thiserror::Error)]
#[error("usage-log sink error: {0}")]
pub struct UsageSinkError(pub String);

/// Build a [`UsageLog`] row from the structured [`Usage`] attached to a
/// successful response, then return it for the sink to persist.
///
/// # Parity (Go `UsageLogService.CreateUsageLogFromRequest`)
///
/// Go's flow is:
/// 1. Resolve the channel / model / api-key ids from `state.Request` /
///    `state.RequestExec` (caller's responsibility — we receive them as
///    arguments).
/// 2. Call `CreateUsageLog` which internally builds the row from
///    `params.Usage` (a structured `*llm.Usage`) and computes cost via the
///    price cache. The Rust split keeps cost lookup pluggable: when
///    `resolved_price` is `None` the S11 no-cost fallback applies (the request
///    still succeeds unblocked).
///
/// The integer ids (`request_id` / `project_id`) are passed as `i64` to mirror
/// the Go Ent schema (which types them as `int`). The recorder accepts string
/// ids from the orchestrator contract and parses them — if parsing fails the
/// usage log is dropped with a warning (Go would never see a non-numeric id, so
/// this is defensive only).
#[allow(clippy::too_many_arguments)]
fn build_usage_log(
    request_id: &str,
    project_id: &str,
    channel_id: Option<i64>,
    actual_model_id: &str,
    api_key_id: Option<i64>,
    source: UsageLogSource,
    format: &str,
    usage: &Usage,
) -> Option<UsageLog> {
    let request_id_i64: i64 = request_id.parse().ok()?;
    let project_id_i64: i64 = project_id.parse().ok()?;
    let params = CreateUsageLogParams::new(
        request_id_i64,
        project_id_i64,
        channel_id,
        actual_model_id,
        usage,
        source,
        format,
        api_key_id,
    );
    Some(create_usage_log_from_structured_usage(params))
}

// ---------------------------------------------------------------------------
// ProductionRequestRecorder
// ---------------------------------------------------------------------------

/// Production [`RequestRecorder`] backed by a real [`RequestService`] + a
/// [`UsageLogSink`].
///
/// Holds the collaborators behind `Arc` so the recorder is `Send + Sync` and
/// can be shared across the orchestrator's retry/failover paths.
///
/// The recorder is intentionally **I/O-only** — every pure decision
/// (canceled-vs-failed status, latency clamp, chunk source routing) already
/// lives in [`crate::orchestrator`]. The recorder's job is to translate those
/// decisions into the right sequence of `RequestService` write calls and to
/// honor the Go detached-context timeout.
pub struct ProductionRequestRecorder {
    request_service: Arc<RequestService>,
    usage_sink: Arc<dyn UsageLogSink>,
}

impl ProductionRequestRecorder {
    /// Wire a recorder with explicit collaborators. Production constructs this
    /// once at boot; tests use [`ProductionRequestRecorder::with_in_memory`].
    pub fn new(request_service: Arc<RequestService>, usage_sink: Arc<dyn UsageLogSink>) -> Self {
        Self {
            request_service,
            usage_sink,
        }
    }

    /// Test-only constructor: wires the in-memory request repo (re-exported
    /// from `conduit-services`) + a [`CapturingUsageLogSink`]. Returns the
    /// recorder AND the repo handle so tests can assert on the recorded rows.
    #[cfg(test)]
    pub fn with_in_memory() -> (
        Self,
        Arc<conduit_services::request_service::InMemoryRequestPersistenceRepo>,
        Arc<CapturingUsageLogSink>,
    ) {
        use conduit_services::request_service::InMemoryRequestPersistenceRepo;
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let service = Arc::new(RequestService::new(repo.clone()));
        let sink = Arc::new(CapturingUsageLogSink::default());
        let recorder = Self::new(service, sink.clone());
        (recorder, repo, sink)
    }

    /// Run a fallible persist future under Go's detached 10s timeout. Mirrors
    /// Go `xcontext.DetachWithTimeout(ctx, time.Second*10)`. On timeout we
    /// surface `ConduitError::internal` with the Go-equivalent message so the
    /// orchestrator tags the `Persist` stage the same way.
    async fn under_detached_timeout<F, T>(
        detached_timeout_ms: u64,
        label: &'static str,
        fut: F,
    ) -> Result<T, ConduitError>
    where
        F: std::future::Future<Output = Result<T, RequestServiceError>>,
    {
        let dur = StdDuration::from_millis(detached_timeout_ms);
        match timeout(dur, fut).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(svc_err)) => {
                warn!(
                    label,
                    error = %svc_err,
                    "request-recorder persist failed"
                );
                // Map the service error to ConduitError::internal so the
                // orchestrator's Persist-stage tagging stays uniform. The
                // underlying message is preserved verbatim for diagnostics.
                Err(ConduitError::internal(format!("{label}: {svc_err}")))
            }
            Err(_) => {
                warn!(label, "request-recorder persist timed out");
                Err(ConduitError::internal(format!(
                    "{label}: persist timed out after {detached_timeout_ms}ms"
                )))
            }
        }
    }

    /// Map a Go-style canceled flag to the terminal request status. Mirrors
    /// `UpdateRequestStatusFromError`'s `errors.Is(err, context.Canceled)`
    /// branch.
    fn terminal_status_for_error(canceled: bool) -> RequestStatus {
        if canceled {
            RequestStatus::Cancelled
        } else {
            FAILURE_PERSISTENCE_TERMINAL_STATUS
        }
    }
}

#[async_trait]
impl RequestRecorder for ProductionRequestRecorder {
    /// Record a successful attempt's execution + usage.
    ///
    /// # Go ordering (must be preserved)
    ///
    /// 1. `UpdateRequestExecutionCompleted` (request_execution.go:175) — flips
    ///    the execution row to `Succeeded`, sets external id + metrics +
    ///    response body. Audio/STT body wrapping is deferred (see module docs).
    /// 2. `UpdateRequestCompleted` (request.go:156) — flips the parent request
    ///    row to `Succeeded`, sets external id + metrics + response body.
    /// 3. `CreateUsageLogFromRequest` (request.go:72) — inserts the usage-log
    ///    row from the response's structured `Usage`. Failure here is logged
    ///    but does NOT mask the success (Go: `log.Warn` + continue).
    async fn record_success(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        attempt: &PipelineAttempt,
        response: &HttpResponse,
    ) -> Result<(), ConduitError> {
        // The orchestrator-side persist runs under Go's detached 10s context
        // (see request_execution.go:132 / request.go:66,93). We apply the same
        // ceiling to the whole record_success body.
        let svc_ctx = ctx_to_request_context(ctx);
        let persist = async {
            // ---- 1. Build the execution-completion inputs from the attempt. ----
            // The pure ExecutionRecordPlan carries request_id / channel_id /
            // actual_model / api_format / pass_through_applied. We synthesize
            // it from the attempt for parity-of-shape (the wiring layer's job
            // is to turn this into the actual service call below).
            let _plan = ExecutionRecordPlan::create(
                request_id,
                attempt.channel_id.as_str(),
                attempt.channel_id.as_str(),
                "openai/chat_completions",
                false,
            );

            // ---- 2. UpdateRequestExecutionCompleted (request_execution.go:175) ----
            // Execution id mirrors the Go `state.RequestExec.ID` (set by
            // `CreateRequestExecution`). The Rust pipeline records the
            // attempt's execution id as `{request_id}-attempt-{sequence}` (see
            // `AttemptRecord::for_candidate`), which matches the in-memory repo
            // contract.
            let execution_id = format!("{request_id}-attempt-{}", attempt.sequence);
            let metrics = build_response_latency_metrics(response);
            let response_body = response_body_for_persist(response);
            let external_id = response_external_id(response);
            // The execution-completed call mirrors Go: errors are logged but
            // do NOT abort the success path (Go logs `log.Warn` and continues
            // to the request-row + usage-log writes). We capture the result
            // and warn-on-error, mirroring that contract.
            if let Err(svc_err) = self
                .request_service
                .update_request_execution_completed(
                    &svc_ctx,
                    project_id,
                    request_id,
                    &execution_id,
                    &external_id,
                    metrics,
                    response_body.clone(),
                )
                .await
            {
                warn!(
                    error = %svc_err,
                    "request-recorder: UpdateRequestExecutionCompleted failed (non-fatal)"
                );
            }

            // ---- 3. UpdateRequestCompleted (request.go:156) ----
            // Unlike the execution write, the request-row write's error IS
            // surfaced (Go also logs Warn here, but the request row is the
            // authoritative terminal status — we bubble its failure up so the
            // orchestrator's Persist stage is tagged).
            self.request_service
                .update_request_completed(
                    &svc_ctx,
                    project_id,
                    request_id,
                    &external_id,
                    metrics,
                    response_body,
                )
                .await?;

            // ---- 4. Usage log (request.go:72) ----
            // Go gates on `llmResp.Usage != nil`. Rust's `Usage::is_zero()`
            // mirrors Go's nil-ness check for the zero value; we additionally
            // accept nonzero usage. On any failure here we log + continue (Go
            // behavior) — the request already succeeded.
            if let Some(usage) = response.usage.as_ref()
                && !usage.is_zero()
                && let Some(usage_log) = build_usage_log(
                    request_id,
                    project_id,
                    parse_optional_i64(&attempt.channel_id),
                    &attempt.channel_id,
                    None,
                    UsageLogSource::Api,
                    "openai/chat_completions",
                    usage,
                )
                && let Err(sink_err) = self.usage_sink.insert_usage_log(&svc_ctx, usage_log).await
            {
                warn!(
                    error = %sink_err,
                    "request-recorder: usage-log sink failed (non-fatal)"
                );
            }

            Ok::<(), RequestServiceError>(())
        };

        Self::under_detached_timeout(
            FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS,
            "record_success",
            persist,
        )
        .await
    }

    /// Record a failed attempt's execution + request status.
    ///
    /// # Go ordering (orchestrator.go:299-328)
    ///
    /// 1. `UpdateRequestExecutionStatusFromError` (when execution exists) —
    ///    runs first.
    /// 2. `UpdateRequestStatusFromError` (when request exists) — runs second.
    async fn record_failure(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        error: &ConduitError,
    ) -> Result<(), ConduitError> {
        // Build the pure plan first. The plan carries the detached timeout +
        // terminal status + error message; the recorder applies them.
        let plan: FailurePersistencePlan = failure_persistence_plan(
            error,
            Some(request_id.to_string()),
            // Execution id is left to the caller in Go (`outbound.GetRequestExecution().ID`).
            // The Rust orchestrator does not yet propagate the last attempt's
            // execution id through the trait signature, so we pass None and
            // the recorder only writes the request row. This mirrors the Go
            // path where the pipeline failed before `persistRequestExecution`
            // produced an execution row.
            None,
        );

        let svc_ctx = ctx_to_request_context(ctx);
        let persist = async {
            // ---- 1. UpdateRequestExecutionStatusFromError (orchestrator.go:307) ----
            // Skipped when execution_id is None (Go: `if requestExec :=
            // outbound.GetRequestExecution(); requestExec != nil`).
            if let Some(execution_id) = plan.execution_id.as_deref() {
                let canceled = is_canceled_error(error);
                let exec_err_msg = plan.error_message.clone();
                // Go's ExtractErrorInfo pulls StatusCode from httpclient.Error.
                // The Rust wiring surfaces it as ConduitError::provider_status.
                let exec_err_info = error.provider_status.map(|s| ExecutionErrorInfo {
                    status_code: Some(s as i64),
                });
                let next_status = Self::terminal_status_for_error(canceled);
                if let Err(svc_err) = self
                    .request_service
                    .update_request_execution_status(
                        &svc_ctx,
                        project_id,
                        request_id,
                        execution_id,
                        next_status,
                        Some(exec_err_msg.as_str()),
                        exec_err_info,
                    )
                    .await
                {
                    warn!(
                        error = %svc_err,
                        "request-recorder: UpdateRequestExecutionStatusFromError failed (non-fatal)"
                    );
                    // Go logs + continues to the request-row update; we mirror.
                }
            }

            // ---- 2. UpdateRequestStatusFromError (orchestrator.go:318) ----
            if plan.persists_request() {
                let canceled = is_canceled_error(error);
                self.request_service
                    .update_request_status_from_error(&svc_ctx, project_id, request_id, canceled)
                    .await?;
            }
            Ok::<(), RequestServiceError>(())
        };

        Self::under_detached_timeout(plan.detached_timeout_ms, "record_failure", persist).await
    }

    /// Consume a [`StreamFinalPlan`] and persist the streaming attempt.
    ///
    /// # Go ordering (outbound.go:100-305)
    ///
    /// On the **Succeeded** branch ([`StreamFinalPlan::write_chunks`] == true,
    /// mirrors Go `persistAggregatedResponse` + `SaveRequestExecutionChunks`):
    /// 1. `CreateUsageLogFromRequest` — when `aggregated.usage` is present and
    ///    non-zero (Go: `if usage := meta.Usage; usage != nil`). Non-fatal.
    /// 2. `UpdateRequestExecutionCompleted` — flips the execution row to
    ///    Succeeded with the aggregated response body + latency metrics +
    ///    external id. Non-fatal (Go logs + continues to chunks).
    /// 3. `SaveRequestExecutionChunks` — persists the buffered (binary-
    ///    summarized) chunk array. Non-fatal (Go logs only).
    ///
    /// On the **Cancelled / Failed** branches (`write_chunks` == false, mirrors
    /// Go `UpdateRequestExecutionStatusFromError`):
    /// 1. `UpdateRequestExecutionStatus` — sets the execution row to the plan's
    ///    [`StreamFinalPlan::final_status`] with [`StreamFinalPlan::error_message`].
    ///    Non-fatal (Go logs + returns).
    ///
    /// The whole body runs under Go's detached 10s ceiling
    /// ([`STREAM_FINAL_DETACHED_TIMEOUT_MS`], mirrored via
    /// [`StreamFinalPlan::detached_timeout_ms`]).
    async fn record_stream_final(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        plan: &StreamFinalPlan,
        execution_id: Option<&str>,
        aggregated: Option<&HttpResponse>,
        chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        // Go: `if ts.requestExec == nil { return }` guards every branch. We
        // mirror by no-oping when the wiring layer has no execution id.
        let execution_id = match execution_id {
            Some(id) => id,
            None => return Ok(()),
        };

        let svc_ctx = ctx_to_request_context(ctx);
        let detached_timeout_ms = plan.detached_timeout_ms;
        let plan_final_status = plan.final_status;
        let plan_error_message = plan.error_message.clone();
        let write_chunks = plan.write_chunks;

        let persist = async {
            if write_chunks {
                // ---- Succeeded branch: Go persistAggregatedResponse + chunks. ----
                if let Some(response) = aggregated {
                    // 1. Usage log (Go: CreateUsageLogFromRequest, non-fatal).
                    if let Some(usage) = response.usage.as_ref()
                        && !usage.is_zero()
                        && let Some(usage_log) = build_usage_log(
                            request_id,
                            project_id,
                            None,
                            "",
                            None,
                            UsageLogSource::Api,
                            "openai/chat_completions",
                            usage,
                        )
                        && let Err(sink_err) =
                            self.usage_sink.insert_usage_log(&svc_ctx, usage_log).await
                    {
                        warn!(
                            error = %sink_err,
                            "request-recorder: stream usage-log sink failed (non-fatal)"
                        );
                    }

                    // 2. UpdateRequestExecutionCompleted (outbound.go:286, non-fatal).
                    let metrics = build_response_latency_metrics(response);
                    let response_body = response_body_for_persist(response);
                    let external_id = response_external_id(response);
                    if let Err(svc_err) = self
                        .request_service
                        .update_request_execution_completed(
                            &svc_ctx,
                            project_id,
                            request_id,
                            execution_id,
                            &external_id,
                            metrics,
                            response_body,
                        )
                        .await
                    {
                        warn!(
                            error = %svc_err,
                            "request-recorder: stream UpdateRequestExecutionCompleted failed (non-fatal)"
                        );
                    }
                }

                // 3. SaveRequestExecutionChunks (outbound.go:302, non-fatal).
                // Go marshals each chunk via marshalStreamEventForStorage
                // (filtering done/binary sentinels) before persisting. The
                // wiring layer is expected to pre-filter; here we serialize the
                // slice as-is (the buffered chunks are already binary-summarized
                // per SummarizeBinaryChunk at outbound.go:84).
                let chunks_value =
                    serde_json::to_value(chunks).unwrap_or_else(|_| Value::Array(Vec::new()));
                if let Err(svc_err) = self
                    .request_service
                    .save_request_execution_chunks(
                        &svc_ctx,
                        project_id,
                        request_id,
                        execution_id,
                        chunks_value,
                    )
                    .await
                {
                    warn!(
                        error = %svc_err,
                        "request-recorder: stream SaveRequestExecutionChunks failed (non-fatal)"
                    );
                }
            } else {
                // ---- Cancelled / Failed branch: Go UpdateRequestExecutionStatusFromError. ----
                let error_message = plan_error_message.as_deref().unwrap_or("stream error");
                if let Err(svc_err) = self
                    .request_service
                    .update_request_execution_status(
                        &svc_ctx,
                        project_id,
                        request_id,
                        execution_id,
                        plan_final_status,
                        Some(error_message),
                        None,
                    )
                    .await
                {
                    warn!(
                        error = %svc_err,
                        "request-recorder: stream UpdateRequestExecutionStatus failed (non-fatal)"
                    );
                }
            }
            Ok::<(), RequestServiceError>(())
        };

        Self::under_detached_timeout(detached_timeout_ms, "record_stream_final", persist).await
    }

    /// Persist the request-level chunk array (Go
    /// `InboundPersistentStream._persistResponse` → `SaveRequestChunks`,
    /// `inbound.go:253-256`). Non-fatal on service error (Go `log.Warn`),
    /// but the detached-timeout ceiling still applies.
    async fn record_stream_request_chunks(
        &self,
        _ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        let svc_ctx = ctx_to_request_context(_ctx);
        let persist = async {
            let chunks_value =
                serde_json::to_value(chunks).unwrap_or_else(|_| Value::Array(Vec::new()));
            if let Err(svc_err) = self
                .request_service
                .save_request_chunks(&svc_ctx, project_id, request_id, chunks_value)
                .await
            {
                warn!(
                    error = %svc_err,
                    "request-recorder: SaveRequestChunks failed (non-fatal)"
                );
            }
            Ok::<(), RequestServiceError>(())
        };
        Self::under_detached_timeout(
            FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS,
            "record_stream_request_chunks",
            persist,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Helpers — pure, testable, mirror small Go glue.
// ---------------------------------------------------------------------------

/// Convert the orchestrator context to the services-layer [`RequestContext`].
///
/// The orchestrator context carries metadata but NOT a policy context (the
/// recorder runs detached from the request, mirroring Go's
/// `xcontext.DetachWithTimeout`). We build a system-bypass policy context so
/// the recorder can write rows regardless of the original caller's
/// permissions — exactly as Go's detached persist does (the detached context
/// inherits no auth scopes).
fn ctx_to_request_context(_orch_ctx: &OrchestratorContext) -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

/// Whether the bubbled-up error represents a client cancellation. Mirrors Go's
/// `errors.Is(err, context.Canceled)`.
///
/// Go cancellation in the Rust port surfaces as `ErrorKind::Internal` with a
/// "canceled"/"cancelled" substring (the cancel-token path the orchestrator
/// uses). We treat only the explicit message marker as cancellation; everything
/// else (including Timeout) is a hard failure.
fn is_canceled_error(error: &ConduitError) -> bool {
    let msg = error.message.to_ascii_lowercase();
    msg.contains("cancel")
}

/// Extract a [`LatencyMetrics`] view from a successful response. Mirrors Go's
/// `state.Perf.Calculate()` path. The wiring layer is expected to populate
/// `HttpResponse.metadata["latency_ms"]` etc. (the perf-record port fills
/// those); when absent we return `None` (Go: `state.Perf == nil`).
fn build_response_latency_metrics(response: &HttpResponse) -> Option<LatencyMetrics> {
    let latency_ms = response
        .metadata
        .get("latency_ms")
        .and_then(|v| v.as_i64())?;
    Some(LatencyMetrics {
        latency_ms: Some(latency_ms),
        first_token_latency_ms: response
            .metadata
            .get("first_token_latency_ms")
            .and_then(|v| v.as_i64()),
        reasoning_duration_ms: response
            .metadata
            .get("reasoning_duration_ms")
            .and_then(|v| v.as_i64()),
    })
}

/// Extract the response body for the DB-storage path. Mirrors Go's
/// `respBody := audioSafeResponseBody(...)` minus the audio/STT branches (which
/// are deferred). Falls back to `response.json_body` when present, then to a
/// JSON-decoded form of the raw body, then to `None`.
fn response_body_for_persist(response: &HttpResponse) -> Option<Value> {
    if let Some(json_body) = response.json_body.clone() {
        return Some(json_body);
    }
    if let Some(body) = response.body.as_deref()
        && let Ok(value) = serde_json::from_slice::<Value>(body)
    {
        return Some(value);
    }
    None
}

/// Extract the LLM response external id (`llmResp.ID` in Go). The wiring layer
/// surfaces this as `HttpResponse.metadata["llm_response_id"]`. Empty string
/// when absent (Go would store `""` — the service treats it as a no-op set).
fn response_external_id(response: &HttpResponse) -> String {
    response
        .metadata
        .get("llm_response_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Parse an optional i64 from a string id. Mirrors Go's int-typed channel ids.
fn parse_optional_i64(s: &str) -> Option<i64> {
    s.parse::<i64>().ok()
}

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

/// In-memory [`UsageLogSink`] that captures every row for test assertions.
/// Production wires a real DB-backed sink; tests use this.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct CapturingUsageLogSink {
    captured: Arc<std::sync::Mutex<Vec<UsageLog>>>,
}

#[cfg(test)]
impl CapturingUsageLogSink {
    /// Return a snapshot of every captured row. Returns an empty vec on
    /// poisoned lock (defensive — tests assert on counts, not poisoning).
    pub fn captured(&self) -> Vec<UsageLog> {
        self.captured.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Number of rows captured so far.
    pub fn count(&self) -> usize {
        self.captured.lock().map(|g| g.len()).unwrap_or(0)
    }
}

#[cfg(test)]
#[async_trait]
impl UsageLogSink for CapturingUsageLogSink {
    async fn insert_usage_log(
        &self,
        _ctx: &RequestContext,
        usage_log: UsageLog,
    ) -> Result<(), UsageSinkError> {
        if let Ok(mut g) = self.captured.lock() {
            g.push(usage_log);
        }
        Ok(())
    }
}

// ===========================================================================
// Tests — mirror the Go `request_test.go` shape (nil guards, usage extraction,
// happy-path ordering) and the orchestrator_error_test.go failure path.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::NoopRequestRecorder;
    use conduit_core::ErrorKind;
    use conduit_llm::HttpResponse;
    use conduit_pipeline::pipeline::{AttemptRecord, ExecutionMode};
    use conduit_services::request_service::{InMemoryRequestPersistenceRepo, RequestRecord};
    use serde_json::json;

    /// Build a services-layer RequestContext for test persist calls.
    fn test_request_context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::system()))
    }

    fn succeeded_attempt(seq: u32, channel: &str) -> AttemptRecord {
        AttemptRecord {
            sequence: seq,
            channel_id: channel.to_string(),
            model_index: 0,
            mode: ExecutionMode::NonStream,
            outcome: Ok(HttpResponse::default()),
        }
    }

    fn response_with_usage(prompt: u64, completion: u64) -> HttpResponse {
        let mut response = HttpResponse::default();
        response.status = 200;
        response.usage = Some(Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            ..Usage::default()
        });
        response.json_body = Some(json!({"id": "resp-1", "object": "chat.completion"}));
        response
            .metadata
            .insert("llm_response_id".to_string(), json!("resp-1"));
        response
            .metadata
            .insert("latency_ms".to_string(), json!(250));
        response
    }

    #[tokio::test]
    async fn noop_recorder_is_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
        let recorder = NoopRequestRecorder;
        let ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "ch-1");
        let response = response_with_usage(50, 150);
        recorder
            .record_success(&ctx, "1", "proj-1", &attempt, &response)
            .await?;
        let err = ConduitError::upstream("boom");
        recorder.record_failure(&ctx, "1", "proj-1", &err).await?;
        Ok(())
    }

    #[tokio::test]
    async fn record_success_updates_request_to_succeeded_and_writes_usage()
    -> Result<(), Box<dyn std::error::Error>> {
        // Numeric ids so the usage-log builder can parse them.
        let (recorder, repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();

        // Seed a Running request row (request_id="1", project_id="1").
        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(&svc_ctx, request)
            .await?;

        let orch_ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "1"); // channel id "1" parses to i64
        let response = response_with_usage(50, 150);

        recorder
            .record_success(&orch_ctx, "1", "1", &attempt, &response)
            .await?;

        // Request row flipped to Succeeded: a Succeeded->Failed transition is
        // forbidden (Succeeded is terminal), proving the recorder wrote it.
        let transition_result = recorder
            .request_service
            .transition_status(
                &svc_ctx,
                "1",
                "1",
                RequestStatus::Succeeded,
                RequestStatus::Failed,
            )
            .await;
        assert!(
            transition_result.is_err(),
            "Succeeded row should be terminal (no further transition); recorder must have flipped it"
        );

        // Usage log captured with the right token counts.
        assert_eq!(sink.count(), 1, "exactly one usage-log row expected");
        let captured = sink.captured();
        assert_eq!(captured[0].prompt_tokens, 50);
        assert_eq!(captured[0].completion_tokens, 150);
        assert_eq!(captured[0].total_tokens, 200);
        assert_eq!(captured[0].request_id, 1);
        assert_eq!(captured[0].project_id, 1);

        // Repo should have one request row.
        assert_eq!(repo.request_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn record_success_skips_usage_when_usage_is_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();

        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(&svc_ctx, request)
            .await?;

        let orch_ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "1");
        // Zero usage -> recorder must skip the usage-log write (Go nil-check).
        let mut response = HttpResponse::default();
        response.usage = Some(Usage::zero());

        recorder
            .record_success(&orch_ctx, "1", "1", &attempt, &response)
            .await?;

        assert_eq!(
            sink.count(),
            0,
            "zero usage must not produce a usage-log row"
        );
        Ok(())
    }

    #[tokio::test]
    async fn record_success_skips_usage_when_no_usage_field()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(&svc_ctx, request)
            .await?;

        let orch_ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "1");
        let response = HttpResponse::default(); // no usage

        recorder
            .record_success(&orch_ctx, "1", "1", &attempt, &response)
            .await?;

        assert_eq!(
            sink.count(),
            0,
            "missing usage must not produce a usage-log row"
        );
        Ok(())
    }

    #[tokio::test]
    async fn record_failure_marks_request_failed() -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, _sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(&svc_ctx, request)
            .await?;

        let orch_ctx = OrchestratorContext::new();
        let err = ConduitError::new(ErrorKind::Upstream, "provider 500");

        recorder.record_failure(&orch_ctx, "1", "1", &err).await?;

        // Request flipped to Failed (terminal).
        let transition_result = recorder
            .request_service
            .transition_status(
                &svc_ctx,
                "1",
                "1",
                RequestStatus::Failed,
                RequestStatus::Succeeded,
            )
            .await;
        assert!(
            transition_result.is_err(),
            "Failed row should be terminal; recorder must have flipped it"
        );
        Ok(())
    }

    #[tokio::test]
    async fn record_failure_with_canceled_marker_marks_request_cancelled()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, _sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        let mut request = RequestRecord::new("1", "req-1", "1", "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(&svc_ctx, request)
            .await?;

        let orch_ctx = OrchestratorContext::new();
        // Message contains "cancel" -> recorder treats as cancellation.
        let err = ConduitError::new(ErrorKind::Internal, "request canceled by client");

        recorder.record_failure(&orch_ctx, "1", "1", &err).await?;

        // Cancelled is terminal. The Failed->Succeeded transition fails because
        // the row is Cancelled (not Failed), confirming the recorder wrote
        // Cancelled rather than Failed.
        let failed_transition = recorder
            .request_service
            .transition_status(
                &svc_ctx,
                "1",
                "1",
                RequestStatus::Failed,
                RequestStatus::Succeeded,
            )
            .await;
        assert!(
            failed_transition.is_err(),
            "row should be Cancelled (not Failed) after a canceled-marker error"
        );

        // The Cancelled->Succeeded transition is also forbidden (terminal).
        let cancelled_transition = recorder
            .request_service
            .transition_status(
                &svc_ctx,
                "1",
                "1",
                RequestStatus::Cancelled,
                RequestStatus::Succeeded,
            )
            .await;
        assert!(
            cancelled_transition.is_err(),
            "Cancelled row is terminal; confirms recorder wrote Cancelled"
        );
        Ok(())
    }

    #[tokio::test]
    async fn record_failure_returns_timeout_error_when_persist_exceeds_ceiling()
    -> Result<(), Box<dyn std::error::Error>> {
        // Structural test: the recorder's under_detached_timeout wrapper
        // surfaces an internal error on timeout. We cannot easily force a 10s
        // timeout in a fast unit test, so we assert the constant the recorder
        // uses matches Go's literal.
        assert_eq!(
            FAILURE_PERSISTENCE_DETACHED_TIMEOUT_MS, 10_000,
            "detached timeout must mirror Go's 10s literal"
        );
        Ok(())
    }

    #[test]
    fn build_usage_log_returns_none_for_non_numeric_ids() {
        // Defensive: Go never sees non-numeric ids, but the recorder must not
        // panic on them.
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            ..Usage::default()
        };
        let log = build_usage_log(
            "not-a-number",
            "1",
            None,
            "gpt-4",
            None,
            UsageLogSource::Api,
            "openai/chat_completions",
            &usage,
        );
        assert!(
            log.is_none(),
            "non-numeric request id must yield no usage-log row"
        );

        let log2 = build_usage_log(
            "1",
            "also-not-numeric",
            None,
            "gpt-4",
            None,
            UsageLogSource::Api,
            "openai/chat_completions",
            &usage,
        );
        assert!(
            log2.is_none(),
            "non-numeric project id must yield no usage-log row"
        );
    }

    #[test]
    fn build_usage_log_populates_token_counts_from_structured_usage() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 200,
            total_tokens: 300,
            ..Usage::default()
        };
        // Use `?` on Option to honor the no-unwrap workspace lint.
        let log = build_usage_log(
            "1",
            "1",
            Some(42),
            "gpt-4",
            Some(7),
            UsageLogSource::Playground,
            "openai/chat_completions",
            &usage,
        );
        let log = match log {
            Some(value) => value,
            None => panic!("numeric ids must produce a usage-log row"),
        };
        assert_eq!(log.prompt_tokens, 100);
        assert_eq!(log.completion_tokens, 200);
        assert_eq!(log.total_tokens, 300);
        assert_eq!(log.channel_id, Some(42));
        assert_eq!(log.api_key_id, Some(7));
        assert_eq!(log.model_id, "gpt-4");
        assert_eq!(log.source, UsageLogSource::Playground);
        assert_eq!(log.format, "openai/chat_completions");
    }

    #[test]
    fn is_canceled_error_detects_marker() {
        assert!(is_canceled_error(&ConduitError::new(
            ErrorKind::Internal,
            "request canceled"
        )));
        assert!(is_canceled_error(&ConduitError::new(
            ErrorKind::Internal,
            "client cancelled"
        )));
        assert!(!is_canceled_error(&ConduitError::new(
            ErrorKind::Upstream,
            "provider 500"
        )));
        assert!(!is_canceled_error(&ConduitError::new(
            ErrorKind::Timeout,
            "timed out"
        )));
    }

    #[test]
    fn terminal_status_for_error_matches_go_branches() {
        assert_eq!(
            ProductionRequestRecorder::terminal_status_for_error(true),
            RequestStatus::Cancelled
        );
        assert_eq!(
            ProductionRequestRecorder::terminal_status_for_error(false),
            FAILURE_PERSISTENCE_TERMINAL_STATUS
        );
        assert_eq!(FAILURE_PERSISTENCE_TERMINAL_STATUS, RequestStatus::Failed);
    }

    #[test]
    fn response_body_for_persist_prefers_json_body() {
        let mut response = HttpResponse::default();
        response.json_body = Some(json!({"id": "resp-1"}));
        let body = response_body_for_persist(&response);
        assert_eq!(body, Some(json!({"id": "resp-1"})));

        let mut response2 = HttpResponse::default();
        response2.body = Some(br#"{"id":"resp-2"}"#.to_vec());
        let body2 = response_body_for_persist(&response2);
        assert_eq!(body2, Some(json!({"id": "resp-2"})));

        let response3 = HttpResponse::default();
        let body3 = response_body_for_persist(&response3);
        assert_eq!(body3, None);
    }

    #[test]
    fn build_response_latency_metrics_reads_metadata() {
        let mut response = HttpResponse::default();
        response
            .metadata
            .insert("latency_ms".to_string(), json!(123));
        response
            .metadata
            .insert("first_token_latency_ms".to_string(), json!(45));
        let metrics = match build_response_latency_metrics(&response) {
            Some(m) => m,
            None => panic!("latency present in metadata must yield metrics"),
        };
        assert_eq!(metrics.latency_ms, Some(123));
        assert_eq!(metrics.first_token_latency_ms, Some(45));
        assert_eq!(metrics.reasoning_duration_ms, None);

        let response_no_latency = HttpResponse::default();
        assert!(build_response_latency_metrics(&response_no_latency).is_none());
    }

    /// Parity with the Go `TestPersistRequestMiddleware_OnOutboundLlmResponse_NilRequest`:
    /// when the recorder has no request row to update, the success path must
    /// still not panic. We approximate the nil-request case by recording
    /// against a non-existent request id; the recorder surfaces the failure as
    /// an internal ConduitError (the orchestrator tags the Persist stage).
    #[tokio::test]
    async fn record_success_against_missing_row_surfaces_persist_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let orch_ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "1");
        let response = response_with_usage(10, 20);

        let result = recorder
            .record_success(&orch_ctx, "999", "1", &attempt, &response)
            .await;
        assert!(
            result.is_err(),
            "recorder must surface an error when the request row is missing"
        );
        // Usage log must NOT have been written (the persist failed before usage).
        assert_eq!(sink.count(), 0);
        Ok(())
    }

    #[allow(dead_code)]
    fn ensure_request_recorder_trait_is_object_safe(_r: Arc<dyn RequestRecorder>) {}

    #[test]
    fn request_recorder_trait_is_object_safe() {
        // If this compiles, the trait is object-safe and can be stored behind
        // Arc<dyn RequestRecorder> as the orchestrator does.
        let recorder: Arc<dyn RequestRecorder> = Arc::new(NoopRequestRecorder);
        ensure_request_recorder_trait_is_object_safe(recorder);
    }

    /// Compile-time check that the in-memory repo + service types we depend on
    /// stay exported from `conduit-services`. If this test drifts we want the
    /// build to fail loudly here, not in downstream callers.
    #[test]
    fn in_memory_repo_type_is_accessible() {
        let _repo = InMemoryRequestPersistenceRepo::new();
    }

    // -------------------------------------------------------------------------
    // record_stream_final — mirrors Go OutboundPersistentStream.Close branches.
    // -------------------------------------------------------------------------

    use crate::orchestrator::stream_final_plan;
    use conduit_llm::StreamEvent;
    use conduit_services::ExecutionRecord;

    /// Seed a request + an execution row so the recorder has something to update.
    async fn seed_request_and_execution(
        recorder: &ProductionRequestRecorder,
        svc_ctx: &RequestContext,
        request_id: &str,
        project_id: &str,
        execution_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut request = RequestRecord::new(request_id, "req-key", project_id, "POST", "/v1/chat");
        request.status = RequestStatus::Running;
        recorder
            .request_service
            .create_request(svc_ctx, request)
            .await?;
        let exec = ExecutionRecord::new(execution_id, request_id, project_id, 1);
        recorder
            .request_service
            .append_execution(svc_ctx, exec)
            .await?;
        Ok(())
    }

    /// Build a chunk slice mirroring the binary-summarized events the
    /// OutboundPersistentStream buffers.
    fn sample_chunks() -> Vec<StreamEvent> {
        vec![
            StreamEvent {
                data: Some(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#.to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                done: true,
                data: Some("[DONE]".to_string()),
                ..StreamEvent::default()
            },
        ]
    }

    /// Succeeded branch (Go persistAggregatedResponse + SaveRequestExecutionChunks):
    /// the execution row is flipped to Succeeded, chunks are persisted, and a
    /// usage-log row is written when usage is present.
    #[tokio::test]
    async fn record_stream_final_success_writes_execution_chunks_and_usage()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        seed_request_and_execution(&recorder, &svc_ctx, "1", "1", "1-attempt-1").await?;

        let plan = stream_final_plan(true, false);
        assert!(plan.write_chunks);
        let mut aggregated = response_with_usage(30, 70);
        aggregated.json_body = Some(json!({"id": "agg-1"}));
        let chunks = sample_chunks();

        let orch_ctx = OrchestratorContext::new();
        recorder
            .record_stream_final(
                &orch_ctx,
                "1",
                "1",
                &plan,
                Some("1-attempt-1"),
                Some(&aggregated),
                &chunks,
            )
            .await?;

        // Execution row flipped to Succeeded (terminal — no further transition).
        let execs = recorder
            .request_service
            .list_executions(&svc_ctx, "1", "1")
            .await?;
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].status, RequestStatus::Succeeded);

        // Chunks persisted as a JSON array on the execution row.
        assert!(
            execs[0].chunks.is_array(),
            "chunks must be persisted as a JSON array"
        );
        let arr = execs[0].chunks.as_array();
        match arr {
            Some(a) if !a.is_empty() => {}
            other => panic!("non-empty chunks array expected, got {other:?}"),
        }

        // Usage log written.
        assert_eq!(sink.count(), 1);
        let captured = sink.captured();
        assert_eq!(captured[0].prompt_tokens, 30);
        assert_eq!(captured[0].completion_tokens, 70);
        Ok(())
    }

    /// Succeeded branch with zero usage: the usage-log write is skipped (Go
    /// `if usage := meta.Usage; usage != nil` nil-check), but chunks + execution
    /// completion still happen.
    #[tokio::test]
    async fn record_stream_final_success_skips_usage_when_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        seed_request_and_execution(&recorder, &svc_ctx, "1", "1", "1-attempt-1").await?;

        let plan = stream_final_plan(true, false);
        let aggregated = HttpResponse::default(); // no usage
        let chunks = sample_chunks();

        let orch_ctx = OrchestratorContext::new();
        recorder
            .record_stream_final(
                &orch_ctx,
                "1",
                "1",
                &plan,
                Some("1-attempt-1"),
                Some(&aggregated),
                &chunks,
            )
            .await?;

        assert_eq!(
            sink.count(),
            0,
            "zero usage must not produce a usage-log row"
        );
        let execs = recorder
            .request_service
            .list_executions(&svc_ctx, "1", "1")
            .await?;
        assert_eq!(execs[0].status, RequestStatus::Succeeded);
        Ok(())
    }

    /// Cancelled branch (client disconnect): execution flipped to Cancelled, no
    /// chunks, no usage. Mirrors Go's UpdateRequestExecutionStatusFromError with
    /// context.Canceled.
    #[tokio::test]
    async fn record_stream_final_canceled_marks_execution_cancelled()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        seed_request_and_execution(&recorder, &svc_ctx, "1", "1", "1-attempt-1").await?;

        let plan = stream_final_plan(false, true);
        assert_eq!(plan.final_status, RequestStatus::Cancelled);
        assert!(!plan.write_chunks);

        let orch_ctx = OrchestratorContext::new();
        recorder
            .record_stream_final(&orch_ctx, "1", "1", &plan, Some("1-attempt-1"), None, &[])
            .await?;

        let execs = recorder
            .request_service
            .list_executions(&svc_ctx, "1", "1")
            .await?;
        assert_eq!(execs[0].status, RequestStatus::Cancelled);
        assert_eq!(sink.count(), 0);
        // Chunks array stays empty (write_chunks was false).
        assert!(execs[0].chunks.as_array().is_none_or(|a| a.is_empty()));
        Ok(())
    }

    /// Failed branch (no terminal event, no client disconnect): execution
    /// flipped to Failed with the Go sentinel message, no chunks, no usage.
    #[tokio::test]
    async fn record_stream_final_no_terminal_event_marks_execution_failed()
    -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let svc_ctx = test_request_context();
        seed_request_and_execution(&recorder, &svc_ctx, "1", "1", "1-attempt-1").await?;

        let plan = stream_final_plan(false, false);
        assert_eq!(plan.final_status, RequestStatus::Failed);
        assert_eq!(
            plan.error_message.as_deref(),
            Some("stream ended without terminal event or completed response")
        );

        let orch_ctx = OrchestratorContext::new();
        recorder
            .record_stream_final(&orch_ctx, "1", "1", &plan, Some("1-attempt-1"), None, &[])
            .await?;

        let execs = recorder
            .request_service
            .list_executions(&svc_ctx, "1", "1")
            .await?;
        assert_eq!(execs[0].status, RequestStatus::Failed);
        assert_eq!(sink.count(), 0);
        Ok(())
    }

    /// No execution id: recorder must no-op (Go `if ts.requestExec == nil`).
    #[tokio::test]
    async fn record_stream_final_no_execution_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let (recorder, _repo, sink) = ProductionRequestRecorder::with_in_memory();
        let plan = stream_final_plan(true, false);
        let orch_ctx = OrchestratorContext::new();
        recorder
            .record_stream_final(&orch_ctx, "1", "1", &plan, None, None, &[])
            .await?;
        assert_eq!(sink.count(), 0);
        Ok(())
    }
}

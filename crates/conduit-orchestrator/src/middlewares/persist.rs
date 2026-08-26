//! Persistence pipeline middlewares — Rust port of Go `persistRequest`
//! (`orchestrator/request.go:19-197`) and `persistRequestExecution`
//! (`orchestrator/request_execution.go:52-270`).
//!
//! Each middleware owns a single cross-cutting concern (S07):
//!
//! - [`PersistRequestMiddleware`] creates the request row on first inbound,
//!   keeps it `processing` while provider attempts are retried, and transitions
//!   it to `completed` only after the final client response is produced. The
//!   orchestrator owns the terminal `failed` transition after retries exhaust.
//! - [`PersistRequestExecutionMiddleware`] creates the execution row on outbound,
//!   updates it to `completed` with response body/metrics on success, and
//!   `failed` with the error message on error.
//!
//! Both use the `*_unchecked` repo variants under a system-bypass
//! [`RequestContext`] — mirroring Go's detached context that skips auth scopes.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use conduit_core::ConduitError;
use conduit_db::repo::request_execution_repo::{
    self as exec_repo, RequestExecutionRepo, UpdateRequestExecutionInput,
};
use conduit_db::repo::request_repo::{self as req_repo, RequestRepo, UpdateRequestInput};
use conduit_db::row::{RequestExecutionRow, RequestRow};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_llm::{HttpRequest, HttpResponse, LlmRequest};
use conduit_pipeline::middleware::PipelineMiddleware;
use conduit_pipeline::middleware::{BoxEventStream, PipelineContext, PipelineResult};
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

const META_DATA_STORAGE_EXTERNAL: &str = "data_storage_external";

/// One request-scoped default storage snapshot resolved by the production
/// host. Primary storage keeps payloads in PostgreSQL; external storage writes
/// JSON artifacts through [`RequestArtifactStorage::save`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestStorageTarget {
    pub id: String,
    pub external: bool,
}

#[async_trait]
pub trait RequestArtifactStorage: Send + Sync {
    async fn current_default(&self) -> Result<Option<RequestStorageTarget>, String>;

    async fn save(&self, storage_id: &str, key: &str, data: Vec<u8>) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// Helper: build a system-bypass RequestContext (mirrors Go detach-from-auth).
// ---------------------------------------------------------------------------

fn admin_ctx() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

/// Current UTC timestamp as an RFC-3339 string, suitable for the repo input
/// `created_at` / `updated_at` fields that parse ISO strings.
fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn response_body_for_persistence(response: &HttpResponse) -> Option<Value> {
    response.json_body.clone().or_else(|| {
        response
            .body
            .as_deref()
            .and_then(|body| serde_json::from_slice(body).ok())
    })
}

fn response_metric_ms(response: &HttpResponse, key: &str) -> Option<i64> {
    response.metadata.get(key).and_then(Value::as_i64)
}

fn context_metric_ms(ctx: &PipelineContext, key: &str) -> Option<i64> {
    ctx.metadata.get(key).and_then(|value| value.parse().ok())
}

fn storage_flag(ctx: &PipelineContext, key: &str, default: bool) -> bool {
    ctx.metadata
        .get(key)
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(default)
}

fn external_storage_id(ctx: &PipelineContext) -> Option<&str> {
    storage_flag(ctx, META_DATA_STORAGE_EXTERNAL, false)
        .then(|| ctx.metadata.get("data_storage_id").map(String::as_str))
        .flatten()
}

fn request_artifact_key(project_id: &str, request_id: &str, filename: &str) -> String {
    format!("/{project_id}/requests/{request_id}/{filename}")
}

fn execution_artifact_key(
    project_id: &str,
    request_id: &str,
    execution_id: &str,
    filename: &str,
) -> String {
    format!("/{project_id}/requests/{request_id}/executions/{execution_id}/{filename}")
}

fn save_external_json(
    storage: Option<&Arc<dyn RequestArtifactStorage>>,
    storage_id: Option<&str>,
    key: String,
    value: &Value,
    artifact: &'static str,
) {
    let (Some(storage), Some(storage_id)) = (storage, storage_id) else {
        return;
    };
    let data = match serde_json::to_vec(value) {
        Ok(data) => data,
        Err(error) => {
            warn!(%error, %key, artifact, "failed to serialize external request artifact");
            return;
        }
    };
    let storage = Arc::clone(storage);
    let storage_id = storage_id.to_string();
    match run_blocking(async move { storage.save(&storage_id, &key, data).await }) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(%error, artifact, "failed to write external request artifact")
        }
        Err(error) => {
            warn!(%error, artifact, "external request artifact write skipped")
        }
    }
}

fn sanitized_headers(headers: &std::collections::BTreeMap<String, String>) -> Value {
    let mut output = serde_json::Map::new();
    for (name, value) in headers {
        if !is_sensitive_header(name) {
            output.insert(name.to_string(), Value::String(value.clone()));
        }
    }
    Value::Object(output)
}

/// Resolve latency across both metadata contracts and the real middleware
/// order. Response hooks run in reverse registration order, so execution
/// persistence can run before the performance middleware has populated
/// `perf_latency_ms`; in that case the outbound start timestamp is the source
/// of truth.
fn response_latency_ms(ctx: &PipelineContext, response: &HttpResponse) -> Option<i64> {
    response_metric_ms(response, "latency_ms")
        .or_else(|| context_metric_ms(ctx, "perf_latency_ms"))
        .or_else(|| {
            let started_at = context_metric_ms(ctx, "perf_outbound_start_ms")?;
            Some(
                Utc::now()
                    .timestamp_millis()
                    .saturating_sub(started_at)
                    .max(0),
            )
        })
}

/// Serialize an inbound request without persisting client credentials.
///
/// `LlmRequest::extra_headers` contains the original HTTP headers so routing
/// middleware can make an explicit pass-through decision.  It must not be
/// stored verbatim: doing so would put the caller's Conduit API API key (and
/// potentially cookies or provider-style API keys) in `requests.request_body`.
fn request_body_for_persistence(request: &LlmRequest) -> Value {
    let mut value = serde_json::to_value(request).unwrap_or(Value::Null);
    let Some(headers) = value
        .get_mut("extra_headers")
        .and_then(Value::as_object_mut)
    else {
        return value;
    };
    headers.retain(|name, _| !is_sensitive_header(name));
    value
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "x-goog-api-key"
            | "api-key"
    )
}

// ===========================================================================
// PersistRequestMiddleware
// ===========================================================================

/// Pipeline middleware that records the request row in the DB.
///
/// Go parity: `persistRequestMiddleware` (`orchestrator/request.go:19-197`).
///
/// Hooks implemented:
/// - `on_inbound_llm_request` — creates the request row with status `pending`,
///   stores the `request_id` on [`PipelineContext`] so the execution middleware
///   can read it.
/// - `on_inbound_raw_response` — transitions the request to `completed` and
///   stores the final client response body (once per request).
/// - `on_outbound_raw_error` — transitions `pending` to `processing`; the
///   orchestrator marks the request failed only after retries exhaust.
pub struct PersistRequestMiddleware {
    repo: Arc<dyn RequestRepo>,
    storage: Option<Arc<dyn RequestArtifactStorage>>,
}

impl PersistRequestMiddleware {
    pub fn new(repo: Arc<dyn RequestRepo>) -> Self {
        Self {
            repo,
            storage: None,
        }
    }

    pub fn with_artifact_storage(mut self, storage: Arc<dyn RequestArtifactStorage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Build a [`RequestRow`] from the inbound LLM request, mirroring Go's
    /// `CreateRequest` (request.go:41-48).
    fn build_request_row(ctx: &PipelineContext, request: &LlmRequest) -> RequestRow {
        let now = Utc::now();
        let request_id = ctx
            .request_id
            .clone()
            .unwrap_or_else(|| format!("req-{}", now.timestamp_millis()));
        let model_id = request.model.clone().unwrap_or_default();
        let request_body = if storage_flag(ctx, "storage_store_request_body", true)
            && external_storage_id(ctx).is_none()
        {
            request_body_for_persistence(request)
        } else {
            Value::Null
        };
        let request_headers = storage_flag(ctx, "storage_store_request_headers", true)
            .then(|| sanitized_headers(&request.extra_headers));

        RequestRow {
            id: request_id,
            project_id: ctx.metadata.get("project_id").cloned().unwrap_or_default(),
            status: req_repo::STATUS_PENDING.to_string(),
            source: ctx
                .metadata
                .get("source")
                .cloned()
                .unwrap_or_else(|| "api".to_string()),
            model_id,
            format: request.api_format.as_str().to_string(),
            stream: request.stream,
            client_ip: ctx.metadata.get("client_ip").cloned().unwrap_or_default(),
            content_saved: false,
            api_key_id: ctx.metadata.get("api_key_id").cloned(),
            trace_id: ctx.metadata.get("trace_id").cloned(),
            data_storage_id: ctx.metadata.get("data_storage_id").cloned(),
            reasoning_effort: None,
            request_headers,
            request_body,
            response_body: None,
            response_chunks: None,
            channel_id: ctx.metadata.get("channel_id").cloned(),
            external_id: None,
            metrics_latency_ms: None,
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            content_storage_id: None,
            content_storage_key: None,
            content_saved_at: None,
            created_at: now,
            updated_at: now,
        }
    }
}

impl PipelineMiddleware for PersistRequestMiddleware {
    fn name(&self) -> &str {
        "persist-request"
    }

    /// Go `OnInboundLlmRequest` (request.go:36-53): create the request row and
    /// store its id on the pipeline context.
    fn on_inbound_llm_request(
        &self,
        ctx: &mut PipelineContext,
        request: LlmRequest,
    ) -> PipelineResult<LlmRequest> {
        // Go guard: `if m.inbound.state.Request != nil { return }`
        // — skip if we already persisted (e.g. retry reuse).
        if ctx.metadata.contains_key("__persist_request_id") {
            return Ok(request);
        }

        let row = Self::build_request_row(ctx, &request);
        let project_id = row.project_id.clone();
        let external_request_body = (storage_flag(ctx, "storage_store_request_body", true)
            && external_storage_id(ctx).is_some())
        .then(|| request_body_for_persistence(&request));
        let storage_id = external_storage_id(ctx).map(str::to_string);

        let repo = Arc::clone(&self.repo);
        let admin = admin_ctx();

        // The repo operations are async, but the pipeline middleware trait is
        // sync. We use `tokio::task::block_in_place` + `Handle::block_on` to
        // bridge. This matches the pattern the orchestrator already uses for
        // sync-trait adapters over async repos.
        let create_result =
            run_blocking(async move { repo.create_request_unchecked(&admin, row).await });

        match create_result {
            Ok(Ok(created)) => {
                // Stamp the id on context so the execution middleware can read it.
                ctx.metadata
                    .insert("__persist_request_id".to_string(), created.id.clone());
                ctx.metadata
                    .insert("__persist_project_id".to_string(), project_id.clone());
                // Also expose the canonical request_id if not already set.
                if ctx.request_id.is_none() {
                    ctx.request_id = Some(created.id.clone());
                }
                if let Some(body) = external_request_body.as_ref() {
                    save_external_json(
                        self.storage.as_ref(),
                        storage_id.as_deref(),
                        request_artifact_key(&project_id, &created.id, "request_body.json"),
                        body,
                        "request_body",
                    );
                }
            }
            Ok(Err(err)) => {
                warn!(
                    error = %err,
                    "persist-request: failed to create request row"
                );
                // Go returns the error here, aborting the pipeline (a DB refusal
                // to create the request row is a genuine data error).
                return Err(ConduitError::internal(format!(
                    "persist-request: create failed: {err}"
                )));
            }
            Err(bridge) => {
                // Runtime bridge failure is infrastructure, not a data error:
                // log and continue so a transient resource shortage does not
                // fail the user's request (P-26). The request row is simply not
                // recorded this time.
                warn!(
                    error = %bridge,
                    "persist-request: runtime bridge failed; request row not created, continuing"
                );
            }
        }

        Ok(request)
    }

    /// Go `OnInboundRawResponse` (request.go:80-162): update the request with
    /// the final client response body and transition to `completed`. Unlike an
    /// outbound response hook, this runs only after the whole attempt loop has
    /// produced a successful response.
    fn on_inbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        let request_id = match ctx.metadata.get("__persist_request_id") {
            Some(id) => id.clone(),
            None => return Ok(response),
        };
        let project_id = ctx
            .metadata
            .get("__persist_project_id")
            .cloned()
            .unwrap_or_default();

        let repo = Arc::clone(&self.repo);
        let admin = admin_ctx();

        // A non-2xx upstream status means the execution failed even though a
        // response body came back (e.g. provider 401/429/5xx). Mirror Go, which
        // marks the request `failed` for any non-success execution rather than
        // `completed`. A 2xx follows the normal completion transition.
        let terminal_status = if (200..300).contains(&response.status) {
            req_repo::STATUS_COMPLETED
        } else {
            req_repo::STATUS_FAILED
        };
        let response_body = storage_flag(ctx, "storage_store_response_body", true)
            .then(|| response_body_for_persistence(&response))
            .flatten();
        let external_response_body = external_storage_id(ctx).and_then(|_| response_body.clone());
        if let Some(body) = external_response_body.as_ref() {
            save_external_json(
                self.storage.as_ref(),
                external_storage_id(ctx),
                request_artifact_key(&project_id, &request_id, "response_body.json"),
                body,
                "response_body",
            );
        }
        let response_body = external_storage_id(ctx)
            .is_none()
            .then_some(response_body)
            .flatten();
        let latency_ms = response_latency_ms(ctx, &response);
        let first_token_ms = response_metric_ms(&response, "first_token_latency_ms")
            .or_else(|| context_metric_ms(ctx, "first_token_latency_ms"));
        let reasoning_ms = response_metric_ms(&response, "reasoning_duration_ms")
            .or_else(|| context_metric_ms(ctx, "reasoning_duration_ms"));

        // Transition pending -> processing -> {completed|failed}. In the Go flow,
        // the request starts as pending from CreateRequest and is moved through
        // processing at execution time. For the simplified middleware port we
        // transition pending -> processing, then processing -> terminal.
        let to_processing = run_blocking({
            let repo = Arc::clone(&repo);
            let admin_inner = admin_ctx();
            let pid = project_id.clone();
            let rid = request_id.clone();
            async move {
                repo.transition_request_status_unchecked(
                    &admin_inner,
                    &pid,
                    &rid,
                    req_repo::STATUS_PENDING,
                    req_repo::STATUS_PROCESSING,
                )
                .await
            }
        });
        log_transition_outcome(
            to_processing,
            &request_id,
            req_repo::STATUS_PENDING,
            req_repo::STATUS_PROCESSING,
        );

        // Persist the final channel before publishing the terminal status. A
        // concurrent trace lookup must never observe a completed row whose
        // channel is still waiting to be written.
        let update = UpdateRequestInput {
            response_body,
            response_chunks: None,
            channel_id: ctx.metadata.get("channel_id").cloned(),
            metrics_latency_ms: latency_ms,
            metrics_first_token_latency_ms: first_token_ms,
            metrics_reasoning_duration_ms: reasoning_ms,
            updated_at: now_rfc3339(),
        };
        let update_result = run_blocking({
            let repo = Arc::clone(&repo);
            let pid = project_id.clone();
            let rid = request_id.clone();
            async move {
                repo.update_request_unchecked(&admin_ctx(), &pid, &rid, update)
                    .await
            }
        });
        match update_result {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => warn!(
                error = %err,
                "persist-request: failed to persist response/channel/metrics"
            ),
            Err(bridge) => warn!(
                error = %bridge,
                "persist-request: response/channel/metrics skipped (runtime bridge failed)"
            ),
        }

        let to_terminal = run_blocking({
            let repo = Arc::clone(&repo);
            let pid = project_id.clone();
            let rid = request_id.clone();
            async move {
                repo.transition_request_status_unchecked(
                    &admin,
                    &pid,
                    &rid,
                    req_repo::STATUS_PROCESSING,
                    terminal_status,
                )
                .await
            }
        });
        log_transition_outcome(
            to_terminal,
            &request_id,
            req_repo::STATUS_PROCESSING,
            terminal_status,
        );

        Ok(response)
    }

    fn on_inbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        let Some(request_id) = ctx.metadata.get("__persist_request_id").cloned() else {
            return Ok(stream);
        };
        let project_id = ctx
            .metadata
            .get("__persist_project_id")
            .cloned()
            .unwrap_or_default();
        let store_chunks = storage_flag(ctx, "storage_store_chunks", false);
        let storage_id = external_storage_id(ctx).map(str::to_string);
        let storage = self.storage.clone();
        let repo = Arc::clone(&self.repo);
        let mut stream = stream;
        let mut chunks = Vec::<Value>::new();
        let mut flushed = false;

        Ok(Box::new(std::iter::from_fn(move || match stream.next() {
            Some(event) => {
                if store_chunks && let Ok(value) = serde_json::to_value(&event) {
                    chunks.push(value);
                }
                Some(event)
            }
            None if !flushed => {
                flushed = true;
                let chunks = std::mem::take(&mut chunks);
                if store_chunks {
                    if storage_id.is_some() {
                        save_external_json(
                            storage.as_ref(),
                            storage_id.as_deref(),
                            request_artifact_key(&project_id, &request_id, "response_chunks.json"),
                            &Value::Array(chunks),
                            "response_chunks",
                        );
                    } else {
                        let repo = Arc::clone(&repo);
                        let pid = project_id.clone();
                        let rid = request_id.clone();
                        let _ = run_blocking(async move {
                            repo.update_request_unchecked(
                                &admin_ctx(),
                                &pid,
                                &rid,
                                UpdateRequestInput {
                                    response_body: None,
                                    response_chunks: Some(Value::Array(chunks)),
                                    channel_id: None,
                                    metrics_latency_ms: None,
                                    metrics_first_token_latency_ms: None,
                                    metrics_reasoning_duration_ms: None,
                                    updated_at: now_rfc3339(),
                                },
                            )
                            .await
                        });
                    }
                }
                let repo = Arc::clone(&repo);
                let pid = project_id.clone();
                let rid = request_id.clone();
                let _ = run_blocking(async move {
                    let _ = repo
                        .transition_request_status_unchecked(
                            &admin_ctx(),
                            &pid,
                            &rid,
                            req_repo::STATUS_PENDING,
                            req_repo::STATUS_PROCESSING,
                        )
                        .await;
                    repo.transition_request_status_unchecked(
                        &admin_ctx(),
                        &pid,
                        &rid,
                        req_repo::STATUS_PROCESSING,
                        req_repo::STATUS_COMPLETED,
                    )
                    .await
                });
                None
            }
            None => None,
        })))
    }

    /// A failed provider attempt means the request has started processing, but
    /// it is not necessarily terminal: the pipeline decides whether to retry
    /// only after raw-error hooks return. The orchestrator's final error branch
    /// owns `processing -> failed` once all retries are exhausted.
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, _error: &ConduitError) {
        let request_id = match ctx.metadata.get("__persist_request_id") {
            Some(id) => id.clone(),
            None => return,
        };
        let project_id = ctx
            .metadata
            .get("__persist_project_id")
            .cloned()
            .unwrap_or_default();

        let repo = Arc::clone(&self.repo);

        // Try pending -> processing first (may already be there).
        let to_processing = run_blocking({
            let repo = Arc::clone(&repo);
            let admin = admin_ctx();
            let pid = project_id.clone();
            let rid = request_id.clone();
            async move {
                repo.transition_request_status_unchecked(
                    &admin,
                    &pid,
                    &rid,
                    req_repo::STATUS_PENDING,
                    req_repo::STATUS_PROCESSING,
                )
                .await
            }
        });
        log_transition_outcome(
            to_processing,
            &request_id,
            req_repo::STATUS_PENDING,
            req_repo::STATUS_PROCESSING,
        );
    }
}

// ===========================================================================
// PersistRequestExecutionMiddleware
// ===========================================================================

/// Pipeline middleware that records the request execution row in the DB.
///
/// Go parity: `persistRequestExecutionMiddleware`
/// (`orchestrator/request_execution.go:52-270`).
///
/// Hooks implemented:
/// - `on_outbound_raw_request` — creates the execution row with status
///   `processing`.
/// - `on_outbound_raw_response` — updates the execution with response body,
///   status `completed`, and latency metrics.
/// - `on_outbound_raw_error` — updates the execution with status `failed` and
///   the error message.
pub struct PersistRequestExecutionMiddleware {
    repo: Arc<dyn RequestExecutionRepo>,
    storage: Option<Arc<dyn RequestArtifactStorage>>,
}

impl PersistRequestExecutionMiddleware {
    pub fn new(repo: Arc<dyn RequestExecutionRepo>) -> Self {
        Self {
            repo,
            storage: None,
        }
    }

    pub fn with_artifact_storage(mut self, storage: Arc<dyn RequestArtifactStorage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Build a [`RequestExecutionRow`] from the outbound HTTP request.
    /// Mirrors Go `CreateRequestExecution` (request.go:323-344).
    fn build_execution_row(ctx: &PipelineContext, request: &HttpRequest) -> RequestExecutionRow {
        let now = Utc::now();
        let request_id = ctx
            .metadata
            .get("__persist_request_id")
            .cloned()
            .unwrap_or_else(|| ctx.request_id.clone().unwrap_or_default());
        // SQL backends allocate their own integer id, while the in-memory repo
        // honors this caller-supplied value. A UUID keeps rapid zero-delay
        // retries distinct even when several attempts start in one millisecond.
        let execution_id = format!("{}-exec-{}", request_id, Uuid::new_v4().simple());
        let project_id = ctx
            .metadata
            .get("__persist_project_id")
            .cloned()
            .unwrap_or_default();
        let channel_id = ctx.metadata.get("channel_id").cloned();
        let model_id = ctx
            .metadata
            .get("actual_model")
            .or_else(|| ctx.metadata.get("model_id"))
            .or_else(|| ctx.metadata.get("request_model"))
            .cloned()
            .unwrap_or_default();
        let format = ctx
            .metadata
            .get("format")
            .or_else(|| ctx.metadata.get("api_format"))
            .cloned()
            .unwrap_or_else(|| "openai/chat_completions".to_string());

        let request_body = request.json_body.clone().unwrap_or_else(|| {
            request
                .body
                .as_deref()
                .and_then(|b| serde_json::from_slice(b).ok())
                .unwrap_or(Value::Null)
        });

        let request_body = if storage_flag(ctx, "storage_store_request_body", true)
            && external_storage_id(ctx).is_none()
        {
            request_body
        } else {
            Value::Null
        };
        let request_headers = storage_flag(ctx, "storage_store_request_headers", true)
            .then(|| sanitized_headers(&request.headers));

        RequestExecutionRow {
            id: execution_id,
            project_id,
            request_id,
            channel_id,
            credential_identity: ctx.metadata.get("credential_identity").cloned(),
            data_storage_id: ctx.metadata.get("data_storage_id").cloned(),
            external_id: None,
            model_id,
            format,
            request_body,
            response_body: None,
            response_chunks: None,
            error_message: None,
            response_status_code: None,
            // Go hard-codes processing on create (request.go:330).
            status: exec_repo::STATUS_PROCESSING.to_string(),
            stream: request
                .json_body
                .as_ref()
                .and_then(|b| b.get("stream"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            metrics_latency_ms: None,
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            request_headers,
            request_url: request.url.clone(),
            pass_through_applied: false,
            created_at: now,
            updated_at: now,
        }
    }
}

impl PipelineMiddleware for PersistRequestExecutionMiddleware {
    fn name(&self) -> &str {
        "persist-request-execution"
    }

    /// Go `OnOutboundRawRequest` (request_execution.go:70-118): create the
    /// execution row with status `processing`.
    fn on_outbound_raw_request(
        &self,
        ctx: &mut PipelineContext,
        request: HttpRequest,
    ) -> PipelineResult<HttpRequest> {
        // This hook runs once per provider attempt. Always create a fresh row;
        // PipelineContext is request-scoped and deliberately reused across
        // retries, so guarding on the previous attempt's id would collapse all
        // attempts into one execution and leak its error fields into success.
        let row = Self::build_execution_row(ctx, &request);
        let project_id = row.project_id.clone();
        let request_id = row.request_id.clone();
        let external_request_body = if storage_flag(ctx, "storage_store_request_body", true)
            && external_storage_id(ctx).is_some()
        {
            Some(request.json_body.clone().unwrap_or_else(|| {
                request
                    .body
                    .as_deref()
                    .and_then(|body| serde_json::from_slice(body).ok())
                    .unwrap_or(Value::Null)
            }))
        } else {
            None
        };
        let storage_id = external_storage_id(ctx).map(str::to_string);

        let repo = Arc::clone(&self.repo);
        let admin = admin_ctx();

        let create_result =
            run_blocking(async move { repo.create_request_execution_unchecked(&admin, row).await });

        match create_result {
            Ok(Ok(created)) => {
                ctx.metadata
                    .insert("__persist_execution_id".to_string(), created.id.clone());
                ctx.metadata
                    .insert("__persist_exec_project_id".to_string(), project_id);
                if let Some(body) = external_request_body.as_ref() {
                    save_external_json(
                        self.storage.as_ref(),
                        storage_id.as_deref(),
                        execution_artifact_key(
                            &created.project_id,
                            &request_id,
                            &created.id,
                            "request_body.json",
                        ),
                        body,
                        "execution_request_body",
                    );
                }
            }
            Ok(Err(err)) => {
                warn!(
                    error = %err,
                    "persist-request-execution: failed to create execution row"
                );
                return Err(ConduitError::internal(format!(
                    "persist-request-execution: create failed: {err}"
                )));
            }
            Err(bridge) => {
                // Infrastructure failure — do not fail the request (P-26).
                warn!(
                    error = %bridge,
                    "persist-request-execution: runtime bridge failed; execution row not created, continuing"
                );
            }
        }

        Ok(request)
    }

    /// Go `OnOutboundRawResponse` (request_execution.go:120-123): stash the
    /// raw response so the LLM response hook can persist it with metrics.
    /// For the simplified port we update the execution directly here.
    fn on_outbound_raw_response(
        &self,
        ctx: &mut PipelineContext,
        response: HttpResponse,
    ) -> PipelineResult<HttpResponse> {
        let execution_id = match ctx.metadata.get("__persist_execution_id") {
            Some(id) => id.clone(),
            None => return Ok(response),
        };
        let project_id = ctx
            .metadata
            .get("__persist_exec_project_id")
            .cloned()
            .unwrap_or_default();

        let response_body = storage_flag(ctx, "storage_store_response_body", true)
            .then(|| response_body_for_persistence(&response))
            .flatten();
        if external_storage_id(ctx).is_some()
            && let Some(body) = response_body.as_ref()
        {
            let request_id = ctx
                .metadata
                .get("__persist_request_id")
                .cloned()
                .unwrap_or_default();
            save_external_json(
                self.storage.as_ref(),
                external_storage_id(ctx),
                execution_artifact_key(
                    &project_id,
                    &request_id,
                    &execution_id,
                    "response_body.json",
                ),
                body,
                "execution_response_body",
            );
        }
        let response_body = external_storage_id(ctx)
            .is_none()
            .then_some(response_body)
            .flatten();

        // Accept the response-metadata contract used by recorders, the
        // context-metadata contract used by PerformanceRecordingMiddleware,
        // and (because response hooks are reverse-ordered) its outbound start
        // timestamp when performance has not run its response hook yet.
        let latency_ms = response_latency_ms(ctx, &response);
        if let Some(latency_ms) = latency_ms {
            ctx.metadata
                .entry("perf_latency_ms".to_string())
                .or_insert_with(|| latency_ms.to_string());
        }
        let first_token_ms = response_metric_ms(&response, "first_token_latency_ms")
            .or_else(|| context_metric_ms(ctx, "first_token_latency_ms"));
        let reasoning_ms = response_metric_ms(&response, "reasoning_duration_ms")
            .or_else(|| context_metric_ms(ctx, "reasoning_duration_ms"));

        // A non-2xx upstream status is a failed execution even with a response
        // body; record the status code and mark it failed (Go parity). A 2xx
        // completes normally.
        let (exec_status, exec_status_code) = if (200..300).contains(&response.status) {
            (exec_repo::STATUS_COMPLETED, None)
        } else {
            (exec_repo::STATUS_FAILED, Some(i64::from(response.status)))
        };

        let update = UpdateRequestExecutionInput {
            status: Some(exec_status.to_string()),
            response_body,
            response_status_code: exec_status_code,
            metrics_latency_ms: latency_ms,
            metrics_first_token_latency_ms: first_token_ms,
            metrics_reasoning_duration_ms: reasoning_ms,
            updated_at: now_rfc3339(),
            ..Default::default()
        };

        let repo = Arc::clone(&self.repo);
        let admin = admin_ctx();

        let update_result = run_blocking(async move {
            repo.update_request_execution_unchecked(&admin, &project_id, &execution_id, update)
                .await
        });

        match update_result {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => warn!(
                error = %err,
                "persist-request-execution: failed to update execution to completed"
            ),
            Err(bridge) => warn!(
                error = %bridge,
                "persist-request-execution: update to completed skipped (runtime bridge failed)"
            ),
        }

        Ok(response)
    }

    fn on_outbound_raw_stream(
        &self,
        ctx: &mut PipelineContext,
        stream: BoxEventStream,
    ) -> PipelineResult<BoxEventStream> {
        if !storage_flag(ctx, "storage_store_chunks", false) {
            return Ok(stream);
        }
        let Some(execution_id) = ctx.metadata.get("__persist_execution_id").cloned() else {
            return Ok(stream);
        };
        let project_id = ctx
            .metadata
            .get("__persist_exec_project_id")
            .cloned()
            .unwrap_or_default();
        let repo = Arc::clone(&self.repo);
        let storage = self.storage.clone();
        let storage_id = external_storage_id(ctx).map(str::to_string);
        let request_id = ctx
            .metadata
            .get("__persist_request_id")
            .cloned()
            .unwrap_or_default();
        let mut stream = stream;
        let mut chunks = Vec::<Value>::new();
        let mut flushed = false;
        Ok(Box::new(std::iter::from_fn(move || match stream.next() {
            Some(event) => {
                if let Ok(value) = serde_json::to_value(&event) {
                    chunks.push(value);
                }
                Some(event)
            }
            None if !flushed => {
                flushed = true;
                let chunks_to_store = std::mem::take(&mut chunks);
                let repo = Arc::clone(&repo);
                let pid = project_id.clone();
                let eid = execution_id.clone();
                if storage_id.is_some() {
                    save_external_json(
                        storage.as_ref(),
                        storage_id.as_deref(),
                        execution_artifact_key(&pid, &request_id, &eid, "response_chunks.json"),
                        &Value::Array(chunks_to_store),
                        "execution_response_chunks",
                    );
                    let _ = run_blocking(async move {
                        repo.update_request_execution_unchecked(
                            &admin_ctx(),
                            &pid,
                            &eid,
                            exec_repo::UpdateRequestExecutionInput {
                                status: Some(exec_repo::STATUS_COMPLETED.to_string()),
                                updated_at: now_rfc3339(),
                                ..Default::default()
                            },
                        )
                        .await
                    });
                } else {
                    let _ = run_blocking(async move {
                        repo.update_request_execution_unchecked(
                            &admin_ctx(),
                            &pid,
                            &eid,
                            exec_repo::UpdateRequestExecutionInput {
                                status: Some(exec_repo::STATUS_COMPLETED.to_string()),
                                response_chunks: Some(chunks_to_store),
                                updated_at: now_rfc3339(),
                                ..Default::default()
                            },
                        )
                        .await
                    });
                }
                None
            }
            None => None,
        })))
    }

    /// Go `OnOutboundRawError` (request_execution.go:189-228): update the
    /// execution row with status `failed` and the error message.
    fn on_outbound_raw_error(&self, ctx: &mut PipelineContext, error: &ConduitError) {
        let execution_id = match ctx.metadata.get("__persist_execution_id") {
            Some(id) => id.clone(),
            None => return,
        };
        let project_id = ctx
            .metadata
            .get("__persist_exec_project_id")
            .cloned()
            .unwrap_or_default();

        let error_message = error.message.clone();
        let status_code = error.provider_status.map(|s| s as i64);

        let update = UpdateRequestExecutionInput {
            status: Some(exec_repo::STATUS_FAILED.to_string()),
            error_message: Some(error_message),
            response_status_code: status_code,
            updated_at: now_rfc3339(),
            ..Default::default()
        };

        let repo = Arc::clone(&self.repo);
        let admin = admin_ctx();

        let update_result = run_blocking(async move {
            repo.update_request_execution_unchecked(&admin, &project_id, &execution_id, update)
                .await
        });

        match update_result {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => warn!(
                error = %err,
                "persist-request-execution: failed to update execution to failed"
            ),
            Err(bridge) => warn!(
                error = %bridge,
                "persist-request-execution: update to failed skipped (runtime bridge failed)"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Async-to-sync bridge.
// ---------------------------------------------------------------------------

/// Run an async block synchronously. Used to bridge the sync
/// `PipelineMiddleware` trait hooks with the async repo methods.
///
/// Tries `tokio::runtime::Handle::try_current()` first; if a runtime is
/// available it uses `block_in_place` + `block_on`. When no runtime is
/// available (unlikely in production, possible in some test setups) it creates a
/// throwaway `Runtime`.
fn run_blocking<F, T>(fut: F) -> Result<T, RuntimeBridgeError>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            Ok(tokio::task::block_in_place(|| handle.block_on(fut)))
        }
        Ok(_) => std::thread::spawn(move || {
            // A current-thread runtime is needed to drive the future off the
            // async context. Creation can fail under resource exhaustion
            // (threads / file descriptors) — precisely the moment persistence
            // must degrade, not amplify the failure. Return an error so the
            // caller logs it and lets the business request succeed, rather than
            // panicking into a 500 (P-26).
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    return Err(RuntimeBridgeError(format!(
                        "persist middleware runtime creation failed: {error}"
                    )));
                }
            };
            Ok(runtime.block_on(fut))
        })
        .join()
        .unwrap_or_else(|_| {
            Err(RuntimeBridgeError(
                "persist middleware runtime thread panicked".to_string(),
            ))
        }),
        Err(_) => {
            // Fallback for contexts without a running tokio runtime.
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(error) => {
                    return Err(RuntimeBridgeError(format!(
                        "persist middleware: cannot create tokio runtime: {error}"
                    )));
                }
            };
            Ok(rt.block_on(fut))
        }
    }
}

/// Failure to bridge the sync middleware trait onto the async repo runtime.
///
/// Kept intentionally simple (a message string): callers only ever log it and
/// continue — a persistence bridge failure must never fail the business request
/// (Go persists on a detached context for the same reason). See P-26.
#[derive(Debug)]
struct RuntimeBridgeError(String);

impl std::fmt::Display for RuntimeBridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Log the outcome of a best-effort request-status transition without failing
/// the request (P-27).
///
/// The four terminal transitions (pending→processing→completed/failed) are
/// best-effort by design — persistence must not fail the business request. But
/// a `let _ =` that discards the result also discards the *reason*, so a failed
/// transition left the request row stuck in `pending`/`processing` with no
/// trace (dashboard shows "processing" forever, GC never reclaims it). This
/// helper keeps the best-effort semantics but makes every failure observable,
/// distinguishing a runtime-bridge failure from a repo-level one. Go logs the
/// same via `logger.Error` on its detached persistence path.
fn log_transition_outcome(
    result: Result<conduit_db::repo::RepoResult<Option<RequestRow>>, RuntimeBridgeError>,
    request_id: &str,
    from: &str,
    to: &str,
) {
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => warn!(
            request_id = %request_id,
            from = %from,
            to = %to,
            error = %err,
            "persist-request: status transition failed"
        ),
        Err(bridge) => warn!(
            request_id = %request_id,
            from = %from,
            to = %to,
            error = %bridge,
            "persist-request: status transition skipped (runtime bridge failed)"
        ),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use conduit_core::ConduitError;
    use conduit_db::repo::request_execution_repo::InMemoryRequestExecutionRepo;
    use conduit_db::repo::request_repo::InMemoryRequestRepo;
    use conduit_llm::{
        ApiFormat, ChatRequest, HttpRequest, HttpResponse, LlmRequest, LlmRequestPayload,
        RequestType,
    };
    use conduit_pipeline::middleware::PipelineContext;
    use conduit_pipeline::pipeline::{
        Executor, Pipeline, PipelineCandidate, RetryPolicy as PipelineRetryPolicy,
    };
    use conduit_transformers::{InboundTransformer, OutboundTransformer, TransformerResult};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct MemoryArtifactStorage {
        writes: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    }

    impl MemoryArtifactStorage {
        fn json(&self, storage_id: &str, key: &str) -> Option<Value> {
            self.writes
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&(storage_id.to_string(), key.to_string()))
                .and_then(|bytes| serde_json::from_slice(bytes).ok())
        }
    }

    #[async_trait]
    impl RequestArtifactStorage for MemoryArtifactStorage {
        async fn current_default(&self) -> Result<Option<RequestStorageTarget>, String> {
            Ok(None)
        }

        async fn save(&self, storage_id: &str, key: &str, data: Vec<u8>) -> Result<(), String> {
            self.writes
                .lock()
                .map_err(|_| "artifact writes lock poisoned".to_string())?
                .insert((storage_id.to_string(), key.to_string()), data);
            Ok(())
        }
    }

    fn admin() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::system()))
    }

    fn dummy_llm_request() -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-4".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn dummy_http_request() -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(json!({"model": "gpt-4", "stream": false})),
            ..HttpRequest::default()
        }
    }

    fn dummy_http_response() -> HttpResponse {
        HttpResponse {
            status: 200,
            json_body: Some(json!({"id": "resp-1", "choices": []})),
            ..HttpResponse::default()
        }
    }

    fn ctx_with_project(request_id: &str, project_id: &str) -> PipelineContext {
        let mut ctx = PipelineContext::new();
        ctx.request_id = Some(request_id.to_string());
        ctx.metadata
            .insert("project_id".to_string(), project_id.to_string());
        ctx.metadata.insert("source".to_string(), "api".to_string());
        ctx.metadata
            .insert("client_ip".to_string(), "127.0.0.1".to_string());
        ctx
    }

    struct RetryTestInbound;

    impl InboundTransformer for RetryTestInbound {
        fn name(&self) -> &'static str {
            "retry-test-inbound"
        }

        fn inbound_request(&self, _request: HttpRequest) -> TransformerResult<LlmRequest> {
            Ok(dummy_llm_request())
        }

        fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
            Ok(response)
        }

        fn inbound_stream_event(
            &self,
            event: conduit_llm::StreamEvent,
        ) -> TransformerResult<conduit_llm::StreamEvent> {
            Ok(event)
        }

        fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
            Err(ConduitError::internal(error.to_string()))
        }
    }

    struct RetryTestOutbound;

    impl OutboundTransformer for RetryTestOutbound {
        fn name(&self) -> &'static str {
            "retry-test-outbound"
        }

        fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
            Ok(HttpRequest {
                method: "POST".to_string(),
                path: "/v1/chat/completions".to_string(),
                json_body: Some(json!({
                    "model": request.model,
                    "stream": request.stream,
                })),
                ..HttpRequest::default()
            })
        }

        fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
            Ok(response)
        }

        fn outbound_stream_event(
            &self,
            event: conduit_llm::StreamEvent,
        ) -> TransformerResult<conduit_llm::StreamEvent> {
            Ok(event)
        }

        fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
            Ok(ConduitError::upstream("provider error").with_provider_status(response.status))
        }
    }

    struct RetryTestExecutor {
        failures_left: AtomicUsize,
    }

    impl RetryTestExecutor {
        fn new(failures: usize) -> Self {
            Self {
                failures_left: AtomicUsize::new(failures),
            }
        }
    }

    #[async_trait]
    impl Executor for RetryTestExecutor {
        async fn execute(&self, _request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
            if self
                .failures_left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
            {
                return Err(ConduitError::upstream("provider 500").with_provider_status(500));
            }
            Ok(HttpResponse {
                status: 200,
                json_body: Some(json!({
                    "id": "retry-success",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "gpt-4",
                    "choices": [],
                })),
                ..HttpResponse::default()
            })
        }

        async fn execute_stream(
            &self,
            _request: &HttpRequest,
        ) -> Result<Vec<conduit_llm::StreamEvent>, ConduitError> {
            Err(ConduitError::internal("unexpected stream attempt"))
        }
    }

    // ---- PersistRequestMiddleware tests ----

    #[tokio::test]
    async fn persist_request_creates_row_on_inbound() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        let request = dummy_llm_request();

        let result = mw.on_inbound_llm_request(&mut ctx, request)?;
        assert_eq!(result.model.as_deref(), Some("gpt-4"));

        // The request row should exist in the repo.
        let admin = admin();
        let found = repo.find_request_by_id_unchecked(&admin, "r-1").await?;
        let row = found.ok_or("request row not found")?;
        assert_eq!(row.id, "r-1");
        assert_eq!(row.project_id, "p-1");
        assert_eq!(row.status, req_repo::STATUS_PENDING);
        assert_eq!(row.model_id, "gpt-4");
        assert_eq!(row.source, "api");
        assert!(!row.stream);

        // Context should carry the persisted id.
        assert_eq!(
            ctx.metadata.get("__persist_request_id").map(String::as_str),
            Some("r-1")
        );
        Ok(())
    }

    #[test]
    fn persisted_request_body_drops_sensitive_headers() -> Result<(), &'static str> {
        let mut request = dummy_llm_request();
        request.extra_headers.insert(
            "Authorization".to_string(),
            "Bearer conduit-secret".to_string(),
        );
        request
            .extra_headers
            .insert("x-api-key".to_string(), "provider-secret".to_string());
        request
            .extra_headers
            .insert("content-type".to_string(), "application/json".to_string());

        let body = request_body_for_persistence(&request);
        let headers = body["extra_headers"]
            .as_object()
            .ok_or("serialized extra_headers object is missing")?;

        assert!(!headers.contains_key("Authorization"));
        assert!(!headers.contains_key("x-api-key"));
        assert_eq!(headers["content-type"], "application/json");
        assert!(!body.to_string().contains("conduit-secret"));
        assert!(!body.to_string().contains("provider-secret"));
        Ok(())
    }

    #[tokio::test]
    async fn persist_request_skips_if_already_persisted() -> Result<(), Box<dyn std::error::Error>>
    {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        // Simulate already persisted.
        ctx.metadata
            .insert("__persist_request_id".to_string(), "r-1".to_string());

        let request = dummy_llm_request();
        let _ = mw.on_inbound_llm_request(&mut ctx, request)?;

        // Repo should be empty — the create was skipped.
        let admin = admin();
        let found = repo.find_request_by_id_unchecked(&admin, "r-1").await?;
        assert!(found.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn persist_request_transitions_to_completed_on_response()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        let _ = mw.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;
        ctx.metadata
            .insert("perf_latency_ms".to_string(), "321".to_string());
        ctx.metadata
            .insert("channel_id".to_string(), "17".to_string());

        let response = dummy_http_response();
        let _ = mw.on_inbound_raw_response(&mut ctx, response)?;

        let admin = admin();
        let row = repo
            .find_request_by_id_unchecked(&admin, "r-1")
            .await?
            .ok_or("request row not found")?;
        assert_eq!(row.status, req_repo::STATUS_COMPLETED);
        assert_eq!(
            row.response_body,
            Some(json!({"id": "resp-1", "choices": []}))
        );
        assert_eq!(row.metrics_latency_ms, Some(321));
        assert_eq!(row.channel_id.as_deref(), Some("17"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_storage_offloads_request_and_execution_bodies()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_repo = Arc::new(InMemoryRequestRepo::new());
        let execution_repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let storage = Arc::new(MemoryArtifactStorage::default());
        let request_middleware = PersistRequestMiddleware::new(request_repo.clone())
            .with_artifact_storage(storage.clone());
        let execution_middleware = PersistRequestExecutionMiddleware::new(execution_repo.clone())
            .with_artifact_storage(storage.clone());
        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("data_storage_id".to_string(), "9".to_string());
        ctx.metadata
            .insert(META_DATA_STORAGE_EXTERNAL.to_string(), "true".to_string());

        request_middleware.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;
        execution_middleware.on_outbound_raw_request(&mut ctx, dummy_http_request())?;
        let execution_id = ctx
            .metadata
            .get("__persist_execution_id")
            .cloned()
            .ok_or("execution id missing")?;
        execution_middleware.on_outbound_raw_response(&mut ctx, dummy_http_response())?;
        request_middleware.on_inbound_raw_response(&mut ctx, dummy_http_response())?;

        let request = request_repo
            .find_request_by_id_unchecked(&admin(), "r-1")
            .await?
            .ok_or("request row missing")?;
        assert_eq!(request.data_storage_id.as_deref(), Some("9"));
        assert!(request.request_body.is_null());
        assert!(request.response_body.is_none());

        let execution = execution_repo
            .find_request_execution_by_id_unchecked(&admin(), &execution_id)
            .await?
            .ok_or("execution row missing")?;
        assert_eq!(execution.data_storage_id.as_deref(), Some("9"));
        assert!(execution.request_body.is_null());
        assert!(execution.response_body.is_none());

        assert!(
            storage
                .json("9", "/p-1/requests/r-1/request_body.json")
                .is_some()
        );
        assert_eq!(
            storage.json("9", "/p-1/requests/r-1/response_body.json"),
            Some(json!({"id": "resp-1", "choices": []}))
        );
        assert_eq!(
            storage.json(
                "9",
                &format!("/p-1/requests/r-1/executions/{execution_id}/request_body.json"),
            ),
            Some(json!({"model": "gpt-4", "stream": false}))
        );
        assert_eq!(
            storage.json(
                "9",
                &format!("/p-1/requests/r-1/executions/{execution_id}/response_body.json"),
            ),
            Some(json!({"id": "resp-1", "choices": []}))
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stream_completion_persists_chunks_and_terminal_statuses()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_repo = Arc::new(InMemoryRequestRepo::new());
        let execution_repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let request_middleware = PersistRequestMiddleware::new(request_repo.clone());
        let execution_middleware = PersistRequestExecutionMiddleware::new(execution_repo.clone());
        let mut ctx = ctx_with_project("r-stream", "p-1");
        ctx.metadata
            .insert("storage_store_chunks".to_string(), "true".to_string());

        request_middleware.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;
        execution_middleware.on_outbound_raw_request(&mut ctx, dummy_http_request())?;
        let execution_id = ctx
            .metadata
            .get("__persist_execution_id")
            .cloned()
            .ok_or("execution id missing")?;
        let event = conduit_llm::StreamEvent {
            event_type: Some("message".to_string()),
            json_data: Some(json!({"delta": "hello"})),
            ..Default::default()
        };

        let outbound = execution_middleware
            .on_outbound_raw_stream(&mut ctx, Box::new(vec![event.clone()].into_iter()))?;
        assert_eq!(outbound.collect::<Vec<_>>(), vec![event.clone()]);
        let inbound = request_middleware
            .on_inbound_raw_stream(&mut ctx, Box::new(vec![event.clone()].into_iter()))?;
        assert_eq!(inbound.collect::<Vec<_>>(), vec![event]);

        let request = request_repo
            .find_request_by_id_unchecked(&admin(), "r-stream")
            .await?
            .ok_or("request row missing")?;
        assert_eq!(request.status, req_repo::STATUS_COMPLETED);
        assert_eq!(
            request
                .response_chunks
                .as_ref()
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );

        let execution = execution_repo
            .find_request_execution_by_id_unchecked(&admin(), &execution_id)
            .await?
            .ok_or("execution row missing")?;
        assert_eq!(execution.status, exec_repo::STATUS_COMPLETED);
        assert_eq!(
            execution
                .response_chunks
                .as_ref()
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        Ok(())
    }

    #[tokio::test]
    async fn persist_request_stays_processing_after_attempt_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        let _ = mw.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;

        let err = ConduitError::upstream("provider 500");
        mw.on_outbound_raw_error(&mut ctx, &err);

        let admin = admin();
        let row = repo
            .find_request_by_id_unchecked(&admin, "r-1")
            .await?
            .ok_or("request row not found")?;
        assert_eq!(row.status, req_repo::STATUS_PROCESSING);
        Ok(())
    }

    #[tokio::test]
    async fn persist_request_error_noop_when_no_request() -> Result<(), Box<dyn std::error::Error>>
    {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo);

        // No prior on_inbound_llm_request — context has no __persist_request_id.
        let mut ctx = PipelineContext::new();
        let err = ConduitError::upstream("provider 500");

        // Should not panic.
        mw.on_outbound_raw_error(&mut ctx, &err);
        Ok(())
    }

    // ---- PersistRequestExecutionMiddleware tests ----

    #[tokio::test]
    async fn persist_execution_creates_row_on_outbound_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("__persist_request_id".to_string(), "r-1".to_string());
        ctx.metadata
            .insert("__persist_project_id".to_string(), "p-1".to_string());
        ctx.metadata
            .insert("channel_id".to_string(), "ch-1".to_string());
        ctx.metadata.insert(
            "credential_identity".to_string(),
            "sha256:credential-fingerprint".to_string(),
        );
        ctx.metadata
            .insert("model_id".to_string(), "gpt-4".to_string());

        let request = dummy_http_request();
        let _ = mw.on_outbound_raw_request(&mut ctx, request)?;

        // Execution row should exist.
        let exec_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("execution id not set")?;

        let admin = admin();
        let found = repo
            .find_request_execution_by_id_unchecked(&admin, exec_id)
            .await?
            .ok_or("execution row not found")?;

        assert_eq!(found.request_id, "r-1");
        assert_eq!(found.project_id, "p-1");
        assert_eq!(found.channel_id.as_deref(), Some("ch-1"));
        assert_eq!(
            found.credential_identity.as_deref(),
            Some("sha256:credential-fingerprint")
        );
        assert_eq!(found.status, exec_repo::STATUS_PROCESSING);
        assert_eq!(found.model_id, "gpt-4");
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_replaces_previous_attempt_id_with_a_new_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("__persist_request_id".to_string(), "r-1".to_string());
        ctx.metadata
            .insert("__persist_project_id".to_string(), "p-1".to_string());
        ctx.metadata.insert(
            "__persist_execution_id".to_string(),
            "e-already".to_string(),
        );

        let request = dummy_http_request();
        let _ = mw.on_outbound_raw_request(&mut ctx, request)?;

        let current_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("new execution id not set")?;
        assert_ne!(current_id, "e-already");
        assert_eq!(
            repo.list_request_executions_unchecked(&admin(), "p-1", "r-1")
                .await?
                .len(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_updates_to_completed_on_response()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("__persist_request_id".to_string(), "r-1".to_string());
        ctx.metadata
            .insert("__persist_project_id".to_string(), "p-1".to_string());
        ctx.metadata
            .insert("model_id".to_string(), "gpt-4".to_string());

        let _ = mw.on_outbound_raw_request(&mut ctx, dummy_http_request())?;
        let exec_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("execution id not set")?
            .clone();

        // Production response hooks run in reverse registration order, so the
        // execution middleware can run before PerformanceRecordingMiddleware
        // has populated perf_latency_ms. Its request hook has already recorded
        // the outbound start, which must be sufficient to persist latency.
        ctx.metadata.insert(
            "perf_outbound_start_ms".to_string(),
            Utc::now()
                .timestamp_millis()
                .saturating_sub(1234)
                .to_string(),
        );
        let mut response = dummy_http_response();
        response
            .metadata
            .insert("first_token_latency_ms".to_string(), json!(120));
        let _ = mw.on_outbound_raw_response(&mut ctx, response)?;

        let admin = admin();
        let row = repo
            .find_request_execution_by_id_unchecked(&admin, &exec_id)
            .await?
            .ok_or("execution row not found")?;
        assert_eq!(row.status, exec_repo::STATUS_COMPLETED);
        assert!(
            row.metrics_latency_ms
                .is_some_and(|latency| latency >= 1234)
        );
        assert_eq!(row.metrics_first_token_latency_ms, Some(120));
        assert!(row.response_body.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_updates_to_failed_on_error() -> Result<(), Box<dyn std::error::Error>>
    {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("__persist_request_id".to_string(), "r-1".to_string());
        ctx.metadata
            .insert("__persist_project_id".to_string(), "p-1".to_string());
        ctx.metadata
            .insert("model_id".to_string(), "gpt-4".to_string());

        let _ = mw.on_outbound_raw_request(&mut ctx, dummy_http_request())?;
        let exec_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("execution id not set")?
            .clone();

        let err = ConduitError::upstream("provider 500").with_provider_status(500);
        mw.on_outbound_raw_error(&mut ctx, &err);

        let admin = admin();
        let row = repo
            .find_request_execution_by_id_unchecked(&admin, &exec_id)
            .await?
            .ok_or("execution row not found")?;
        assert_eq!(row.status, exec_repo::STATUS_FAILED);
        assert_eq!(row.error_message.as_deref(), Some("provider 500"));
        assert_eq!(row.response_status_code, Some(500));
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_error_noop_when_no_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo);

        let mut ctx = PipelineContext::new();
        let err = ConduitError::upstream("boom");

        // Should not panic.
        mw.on_outbound_raw_error(&mut ctx, &err);
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_response_noop_when_no_execution()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo);

        let mut ctx = PipelineContext::new();
        let response = dummy_http_response();

        // Should pass through without error.
        let out = mw.on_outbound_raw_response(&mut ctx, response)?;
        assert_eq!(out.status, 200);
        Ok(())
    }

    #[tokio::test]
    async fn persist_request_name_matches_go() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestRepo::new());
        let mw = PersistRequestMiddleware::new(repo);
        assert_eq!(mw.name(), "persist-request");
        Ok(())
    }

    #[tokio::test]
    async fn persist_execution_name_matches_go() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let mw = PersistRequestExecutionMiddleware::new(repo);
        assert_eq!(mw.name(), "persist-request-execution");
        Ok(())
    }

    // ---- Integration: both middlewares on the same pipeline context ----

    #[tokio::test]
    async fn both_middlewares_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let req_repo = Arc::new(InMemoryRequestRepo::new());
        let exec_repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let req_mw = PersistRequestMiddleware::new(req_repo.clone());
        let exec_mw = PersistRequestExecutionMiddleware::new(exec_repo.clone());

        let mut ctx = ctx_with_project("r-1", "p-1");
        ctx.metadata
            .insert("model_id".to_string(), "gpt-4".to_string());

        // 1. Inbound: create request.
        let _ = req_mw.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;

        // 2. Outbound: create execution.
        let _ = exec_mw.on_outbound_raw_request(&mut ctx, dummy_http_request())?;

        // 3. Response: update both.
        let response = dummy_http_response();
        let _ = exec_mw.on_outbound_raw_response(&mut ctx, response.clone())?;
        let _ = req_mw.on_inbound_raw_response(&mut ctx, response)?;

        let admin = admin();
        let req_row = req_repo
            .find_request_by_id_unchecked(&admin, "r-1")
            .await?
            .ok_or("request row missing")?;
        assert_eq!(req_row.status, req_repo::STATUS_COMPLETED);

        let exec_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("execution id not set")?;
        let exec_row = exec_repo
            .find_request_execution_by_id_unchecked(&admin, exec_id)
            .await?
            .ok_or("execution row missing")?;
        assert_eq!(exec_row.status, exec_repo::STATUS_COMPLETED);
        assert_eq!(exec_row.request_id, "r-1");
        Ok(())
    }

    #[tokio::test]
    async fn both_middlewares_error_path() -> Result<(), Box<dyn std::error::Error>> {
        let req_repo = Arc::new(InMemoryRequestRepo::new());
        let exec_repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let req_mw = PersistRequestMiddleware::new(req_repo.clone());
        let exec_mw = PersistRequestExecutionMiddleware::new(exec_repo.clone());

        let mut ctx = ctx_with_project("r-2", "p-1");
        ctx.metadata
            .insert("model_id".to_string(), "gpt-4".to_string());

        // 1. Inbound: create request.
        let _ = req_mw.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;

        // 2. Outbound: create execution.
        let _ = exec_mw.on_outbound_raw_request(&mut ctx, dummy_http_request())?;

        // 3. Error: fail the attempt, but keep the parent request processing;
        // the orchestrator owns its terminal failed transition after retries.
        let err = ConduitError::upstream("upstream error");
        exec_mw.on_outbound_raw_error(&mut ctx, &err);
        req_mw.on_outbound_raw_error(&mut ctx, &err);

        let admin = admin();
        let req_row = req_repo
            .find_request_by_id_unchecked(&admin, "r-2")
            .await?
            .ok_or("request row missing")?;
        assert_eq!(req_row.status, req_repo::STATUS_PROCESSING);

        let exec_id = ctx
            .metadata
            .get("__persist_execution_id")
            .ok_or("execution id not set")?;
        let exec_row = exec_repo
            .find_request_execution_by_id_unchecked(&admin, exec_id)
            .await?
            .ok_or("execution row missing")?;
        assert_eq!(exec_row.status, exec_repo::STATUS_FAILED);
        assert_eq!(exec_row.error_message.as_deref(), Some("upstream error"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_attempts_persist_independent_executions_and_final_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_repo = Arc::new(InMemoryRequestRepo::new());
        let execution_repo = Arc::new(InMemoryRequestExecutionRepo::new());
        let pipeline = Pipeline::new(
            Arc::new(RetryTestInbound),
            Arc::new(RetryTestOutbound),
            Arc::new(RetryTestExecutor::new(3)),
        )
        .with_retry_policy(PipelineRetryPolicy {
            enabled: true,
            max_channel_retries: 1,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 0,
            empty_response_detection: false,
        })
        .with_middlewares(vec![
            Arc::new(PersistRequestMiddleware::new(request_repo.clone())),
            Arc::new(PersistRequestExecutionMiddleware::new(
                execution_repo.clone(),
            )),
        ]);

        let mut ctx = ctx_with_project("retry-request", "retry-project");
        let inbound = dummy_http_request();
        let (response, attempts) = pipeline
            .process(
                &mut ctx,
                inbound.clone(),
                &inbound,
                &[
                    PipelineCandidate::from("primary"),
                    PipelineCandidate::from("secondary"),
                ],
            )
            .await?;

        assert_eq!(response.status, 200);
        assert_eq!(attempts.len(), 4);
        assert_eq!(
            attempts
                .iter()
                .map(|attempt| attempt.channel_id.as_str())
                .collect::<Vec<_>>(),
            vec!["primary", "primary", "primary", "secondary"]
        );

        let admin = admin();
        let request = request_repo
            .find_request_by_id_unchecked(&admin, "retry-request")
            .await?
            .ok_or("request row missing")?;
        assert_eq!(request.status, req_repo::STATUS_COMPLETED);

        let executions = execution_repo
            .list_request_executions_unchecked(&admin, "retry-project", "retry-request")
            .await?;
        assert_eq!(
            executions.len(),
            4,
            "one execution row per provider attempt"
        );
        assert_eq!(
            executions
                .iter()
                .filter(|row| row.status == exec_repo::STATUS_FAILED)
                .count(),
            3
        );
        for failed in executions
            .iter()
            .filter(|row| row.status == exec_repo::STATUS_FAILED)
        {
            assert_eq!(failed.channel_id.as_deref(), Some("primary"));
            assert_eq!(failed.error_message.as_deref(), Some("provider 500"));
            assert_eq!(failed.response_status_code, Some(500));
        }
        let succeeded = executions
            .iter()
            .find(|row| row.status == exec_repo::STATUS_COMPLETED)
            .ok_or("successful execution missing")?;
        assert_eq!(succeeded.channel_id.as_deref(), Some("secondary"));
        assert_eq!(succeeded.error_message, None);
        assert_eq!(succeeded.response_status_code, None);
        assert!(succeeded.response_body.is_some());
        Ok(())
    }

    // ---- P-26 / P-27 regression: persistence failure must not fail the
    //      business request, and must not be silent ----

    /// A `RequestRepo` whose CREATE succeeds (so the request id is stamped and
    /// the response hook proceeds) but whose status transitions always fail.
    /// Wraps an `InMemoryRequestRepo` and overrides only the transition.
    struct TransitionFailingRepo {
        inner: InMemoryRequestRepo,
    }

    impl TransitionFailingRepo {
        fn new() -> Self {
            Self {
                inner: InMemoryRequestRepo::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl RequestRepo for TransitionFailingRepo {
        async fn create_request_unchecked(
            &self,
            ctx: &RequestContext,
            row: RequestRow,
        ) -> conduit_db::repo::RepoResult<RequestRow> {
            self.inner.create_request_unchecked(ctx, row).await
        }

        async fn transition_request_status_unchecked(
            &self,
            _ctx: &RequestContext,
            _project_id: &str,
            _request_id: &str,
            _expected_status: &str,
            _next_status: &str,
        ) -> conduit_db::repo::RepoResult<Option<RequestRow>> {
            // Simulate a DB failure on the terminal transition (e.g. connection
            // dropped), which the old `let _ =` code silently swallowed.
            Err(conduit_db::repo::RepoError::Database(
                "simulated transition failure".to_string(),
            ))
        }

        // Remaining methods delegate to the inner in-memory repo — the test
        // only exercises create + transition.
        async fn find_request_by_id_unchecked(
            &self,
            ctx: &RequestContext,
            request_id: &str,
        ) -> conduit_db::repo::RepoResult<Option<RequestRow>> {
            self.inner
                .find_request_by_id_unchecked(ctx, request_id)
                .await
        }

        async fn find_request_by_external_id_unchecked(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            external_id: &str,
        ) -> conduit_db::repo::RepoResult<Option<RequestRow>> {
            self.inner
                .find_request_by_external_id_unchecked(ctx, project_id, external_id)
                .await
        }

        async fn list_requests_unchecked(
            &self,
            ctx: &RequestContext,
            query: &conduit_db::repo::request_repo::RequestListQuery,
        ) -> conduit_db::repo::RepoResult<conduit_db::repo::request_repo::RequestListResult>
        {
            self.inner.list_requests_unchecked(ctx, query).await
        }

        async fn update_request_unchecked(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            input: UpdateRequestInput,
        ) -> conduit_db::repo::RepoResult<RequestRow> {
            self.inner
                .update_request_unchecked(ctx, project_id, request_id, input)
                .await
        }

        async fn mark_content_saved_unchecked(
            &self,
            ctx: &RequestContext,
            request_id: &str,
            input: conduit_db::repo::request_repo::ContentSavedInput,
        ) -> conduit_db::repo::RepoResult<RequestRow> {
            self.inner
                .mark_content_saved_unchecked(ctx, request_id, input)
                .await
        }

        async fn reclaim_stale_processing_unchecked(
            &self,
            ctx: &RequestContext,
            cutoff_created_at: &str,
            now: &str,
        ) -> conduit_db::repo::RepoResult<Vec<String>> {
            self.inner
                .reclaim_stale_processing_unchecked(ctx, cutoff_created_at, now)
                .await
        }
    }

    /// P-27: when a status transition fails, the request still succeeds — the
    /// failure is best-effort (logged, not propagated). Before the fix the
    /// failure was silently discarded via `let _ =`; the fix logs it. Here we
    /// assert the response is returned `Ok` regardless (the "not silent" half
    /// is covered by `log_transition_outcome_handles_all_variants`).
    #[tokio::test]
    async fn transition_failure_does_not_fail_the_request() -> Result<(), Box<dyn std::error::Error>>
    {
        let repo = Arc::new(TransitionFailingRepo::new());
        let mw = PersistRequestMiddleware::new(repo);

        // Inbound: create the request row (create succeeds).
        let mut ctx = ctx_with_project("r-1", "p-1");
        let _ = mw.on_inbound_llm_request(&mut ctx, dummy_llm_request())?;

        // Outbound success path drives pending->processing->completed, both of
        // which fail in this repo. The request must still complete Ok.
        let response = mw.on_inbound_raw_response(&mut ctx, dummy_http_response());
        assert!(
            response.is_ok(),
            "a failed status transition must not fail the business request (P-27)"
        );
        Ok(())
    }

    /// P-27: the transition-outcome logger accepts all three result shapes
    /// (success, repo error, runtime-bridge error) without panicking. This
    /// exercises the warn branches that replaced the silent `let _ =`.
    #[test]
    fn log_transition_outcome_handles_all_variants() {
        // Success — no log, no panic.
        log_transition_outcome(Ok(Ok(None)), "r-1", "pending", "processing");
        // Repo error — warns, must not panic.
        log_transition_outcome(
            Ok(Err(conduit_db::repo::RepoError::Database(
                "boom".to_string(),
            ))),
            "r-1",
            "pending",
            "processing",
        );
        // Runtime-bridge error — warns, must not panic.
        log_transition_outcome(
            Err(RuntimeBridgeError("no runtime".to_string())),
            "r-1",
            "processing",
            "completed",
        );
    }

    /// P-26: `run_blocking` returns `Ok` (not a panic) on the happy path from
    /// within a multi-thread runtime, confirming the signature change threads
    /// the success value through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_blocking_returns_ok_on_success() {
        let value = run_blocking(async { 42_i32 });
        assert!(matches!(value, Ok(42)), "run_blocking must yield Ok(42)");
    }
}

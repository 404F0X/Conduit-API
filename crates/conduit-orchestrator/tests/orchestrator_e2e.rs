//! RUST-P9-006 A01/A02/A03 — in-process e2e over a fake provider chain.
//!
//! A01: request → select → transform → provider → response → request/usage
//! log, through `CommandOrchestrator::process_command` with a fake
//! inbound/outbound transformer pair, a fake executor, and the production
//! recorder over the in-memory repo.
//!
//! A02: streaming chain with client cancel through the S38
//! `OutboundForwardingStream` (client drop → upstream cancel + canceled rows).
//!
//! A03: mirrors of the Go `orchestrator_basic_test.go` /
//! `orchestrator_error_test.go` / `orchestrator_streaming_test.go` cases that
//! are portable without real provider transformers / ent DB — each test's doc
//! comment names the Go test it mirrors. Non-portable cases are listed as
//! pending in the task report (never synthesized).
//!
//! # Known breakpoints (asserted-to-the-farthest-station notes)
//!
//! * The Rust pipeline does not yet run the response-side transform chain
//!   (Go `Outbound.TransformResponse` → `Inbound.TransformResponse`); the
//!   fake executor's `HttpResponse` stands in for the transformed response
//!   (S29-adjacent wiring gap).
//! * Request/execution DB rows are seeded by the caller — the Go
//!   `persistRequest.OnInboundLlmRequest` / `persistRequestExecution
//!   .OnOutboundRawRequest` row-creation middlewares are not yet mounted on
//!   the Rust pipeline (the recorder assumes rows exist).
//! * `record_failure` does not receive the failed attempt's execution id, so
//!   the Go `orchestrator_error_test.go` assertion on the execution row's
//!   error message is pending.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_core::objects::channel_settings::ModelMapping;
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_llm::{
    ApiFormat, ChatRequest, HttpRequest, HttpResponse, LlmRequest, LlmRequestPayload, RequestType,
    StreamEvent, Usage,
};
use conduit_orchestrator::candidate::Candidate;
use conduit_orchestrator::candidates::{CandidateRequest, ChannelModelsCandidate};
use conduit_orchestrator::load_balancer::{
    RetryPolicy as LbRetryPolicy, ScoringStrategy, ScoringStrategySet, StaticStickyKeyProvider,
    WeightScoring,
};
use conduit_orchestrator::orchestrator::{
    CandidateSource, CommandOrchestrator, DefaultCandidateProjector, FlagCancelToken,
    OrchestratorContext, OrchestratorStage,
};
use conduit_orchestrator::pre_execution::ModelMapper;
use conduit_orchestrator::request_recorder::{
    ProductionRequestRecorder, UsageLogSink, UsageSinkError,
};
use conduit_pipeline::pipeline::{Executor, Pipeline, RetryHooks, RetryPolicy as PipeRetryPolicy};
use conduit_services::channel_service::{ChannelModelEntry, ModelSource};
use conduit_services::request_service::{
    InMemoryRequestPersistenceRepo, RequestPersistenceRepo, RequestRecord,
};
use conduit_services::usage_service::UsageLog;
use conduit_services::{ExecutionRecord, RequestService, RequestStatus};
use conduit_transformers::{InboundTransformer, OutboundTransformer, TransformerResult};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// Fakes — stand-ins for the Go test doubles in `tester.go` /
// `orchestrator_basic_test.go` (`staticChannelSelector`, `mockExecutor`,
// `sequenceExecutor`, openai transformers).
// ---------------------------------------------------------------------------

/// Static candidate source (Go `staticChannelSelector`, tester.go). Captures
/// the [`CandidateRequest`] it was asked to resolve.
struct FakeSource {
    candidates: Vec<ChannelModelsCandidate>,
    seen_models: Mutex<Vec<String>>,
}

impl FakeSource {
    fn new(candidates: Vec<ChannelModelsCandidate>) -> Self {
        Self {
            candidates,
            seen_models: Mutex::new(Vec::new()),
        }
    }

    fn seen_models(&self) -> Vec<String> {
        self.seen_models
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl CandidateSource for FakeSource {
    async fn select(
        &self,
        request: &CandidateRequest,
    ) -> Result<Vec<ChannelModelsCandidate>, ConduitError> {
        if let Ok(mut seen) = self.seen_models.lock() {
            seen.push(request.model.clone());
        }
        Ok(self.candidates.clone())
    }
}

/// One resolvable candidate (Go `channelsToTestCandidates` over one channel).
fn test_candidate(channel_id: &str, model: &str) -> ChannelModelsCandidate {
    ChannelModelsCandidate {
        channel_id: channel_id.to_string(),
        channel_name: format!("Test Channel {channel_id}"),
        ordering_weight: 0,
        priority: 0,
        models: vec![ChannelModelEntry {
            request_model: model.to_string(),
            actual_model: model.to_string(),
            source: ModelSource::Direct,
        }],
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
        settings: None,
        theoretical_cost_accounting: None,
        cost_efficiency_score: 0,
    }
}

/// Fake inbound transformer (stands in for Go `openai.NewInboundTransformer`
/// immediately followed by the S09 `applyModelMapping` middleware — Go's test
/// harness runs inbound+mapping as one step because the openai inbound does not
/// itself apply API-key profile mappings). Parses `{"model", "stream",
/// "messages"}` and errors when `model` is missing — mirroring the Go openai
/// inbound's "model is required" behavior asserted by
/// `TestChatCompletionOrchestrator_Process_InvalidRequest`. When `mappings` is
/// non-empty, the parsed client model is run through [`ModelMapper`] so the
/// downstream `LlmRequest.model` carries the mapped value (mirroring Go
/// `request.Model = mapped` after `applyModelMapping`).
struct FakeInbound {
    calls: AtomicUsize,
    mappings: Vec<ModelMapping>,
}

impl FakeInbound {
    /// Construct with an API-key profile model-mapping table (Go
    /// `APIKeyProfile.ModelMappings`). Empty `mappings` mirrors Go's default
    /// `ModelMappings == nil` (no rewriting). The mapping runs after the
    /// inbound parses the client model and before select — mirrors Go's
    /// `applyModelMapping` middleware.
    fn with_mappings(mappings: Vec<ModelMapping>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            mappings,
        }
    }
}

impl InboundTransformer for FakeInbound {
    fn name(&self) -> &'static str {
        "fake-openai-inbound"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let body = request.json_body.unwrap_or(Value::Null);
        let client_model = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            // Go openai inbound: a request without a model is invalid (the
            // error test asserts the message mentions "model").
            .ok_or_else(|| ConduitError::invalid_request("model is required"))?;
        // S09 applyModelMapping: first-matching mapping wins; no match leaves
        // the model untouched. Go: `(*ModelMapper).applyModelMapping`.
        let model = if self.mappings.is_empty() {
            client_model.clone()
        } else {
            ModelMapper::map_model_owned(&self.mappings, &client_model)
        };
        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        Ok(LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some(model),
            stream,
            payload: LlmRequestPayload::Chat(ChatRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        })
    }

    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Ok(HttpResponse::default())
    }
}

/// Fake outbound transformer (stands in for Go
/// `openai.NewOutboundTransformer`): emits the provider-facing HTTP request
/// and records the model it saw.
struct FakeOutbound {
    seen_models: Mutex<Vec<String>>,
}

impl FakeOutbound {
    fn new() -> Self {
        Self {
            seen_models: Mutex::new(Vec::new()),
        }
    }

    fn seen_models(&self) -> Vec<String> {
        self.seen_models
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

impl OutboundTransformer for FakeOutbound {
    fn name(&self) -> &'static str {
        "fake-openai-outbound"
    }

    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        let model = request.model.clone().unwrap_or_default();
        if let Ok(mut seen) = self.seen_models.lock() {
            seen.push(model.clone());
        }
        Ok(HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            request_type: Some(request.request_type),
            api_format: Some(request.api_format),
            json_body: Some(json!({
                "model": model,
                "stream": request.stream,
            })),
            ..HttpRequest::default()
        })
    }

    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
        Ok(ConduitError::upstream("fake provider error").with_provider_status(response.status))
    }
}

/// One scripted provider step (Go `executorStep`, orchestrator_basic_test.go:818).
enum Step {
    Ok(HttpResponse),
    Err(ConduitError),
    Stream(Vec<StreamEvent>),
}

/// Scripted executor (Go `mockExecutor` / `sequenceExecutor`). Records every
/// provider-facing request; pops one step per call.
struct FakeExecutor {
    steps: Mutex<Vec<Step>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeExecutor {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn captured_requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn pop_step(&self) -> Result<Step, ConduitError> {
        let mut steps = self
            .steps
            .lock()
            .map_err(|_| ConduitError::internal("steps lock poisoned"))?;
        if steps.is_empty() {
            return Err(ConduitError::internal("no more steps available"));
        }
        Ok(steps.remove(0))
    }
}

#[async_trait]
impl Executor for FakeExecutor {
    async fn execute(&self, request: &HttpRequest) -> Result<HttpResponse, ConduitError> {
        if let Ok(mut reqs) = self.requests.lock() {
            reqs.push(request.clone());
        }
        match self.pop_step()? {
            Step::Ok(response) => Ok(response),
            Step::Err(err) => Err(err),
            Step::Stream(_) => Err(ConduitError::internal("stream step on non-stream call")),
        }
    }

    async fn execute_stream(
        &self,
        request: &HttpRequest,
    ) -> Result<Vec<StreamEvent>, ConduitError> {
        if let Ok(mut reqs) = self.requests.lock() {
            reqs.push(request.clone());
        }
        match self.pop_step()? {
            Step::Stream(events) => Ok(events),
            Step::Err(err) => Err(err),
            Step::Ok(_) => Err(ConduitError::internal("non-stream step on stream call")),
        }
    }
}

/// Capturing usage-log sink (integration-test analog of the crate-internal
/// `CapturingUsageLogSink`; Go asserts on the `usage_logs` table).
#[derive(Default)]
struct CapturingSink {
    rows: Mutex<Vec<UsageLog>>,
}

impl CapturingSink {
    fn rows(&self) -> Vec<UsageLog> {
        self.rows.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl UsageLogSink for CapturingSink {
    async fn insert_usage_log(
        &self,
        _ctx: &RequestContext,
        usage_log: UsageLog,
    ) -> Result<(), UsageSinkError> {
        if let Ok(mut rows) = self.rows.lock() {
            rows.push(usage_log);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness — wires the full CommandOrchestrator over the fakes.
// ---------------------------------------------------------------------------

fn svc_ctx() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

/// The canned OpenAI-shaped provider response (Go `buildMockOpenAIResponse`).
/// `usage`/`metadata` carry what the response-side transform would attach —
/// see the module-docs breakpoint note.
fn mock_openai_response(
    id: &str,
    model: &str,
    content: &str,
    prompt: u64,
    completion: u64,
) -> HttpResponse {
    let mut response = HttpResponse::default();
    response.status = 200;
    response.json_body = Some(json!({
        "id": id,
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt,
            "completion_tokens": completion,
            "total_tokens": prompt + completion
        }
    }));
    response.usage = Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        ..Usage::default()
    });
    response
        .metadata
        .insert("llm_response_id".to_string(), json!(id));
    response
        .metadata
        .insert("latency_ms".to_string(), json!(120));
    response
}

/// The inbound HTTP request (Go `buildTestRequest`).
fn test_request(model: &str, message: &str, stream: bool) -> HttpRequest {
    HttpRequest {
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        json_body: Some(json!({
            "model": model,
            "stream": stream,
            "messages": [{"role": "user", "content": message}],
        })),
        ..HttpRequest::default()
    }
}

struct Harness {
    orchestrator: CommandOrchestrator,
    source: Arc<FakeSource>,
    inbound: Arc<FakeInbound>,
    outbound: Arc<FakeOutbound>,
    executor: Arc<FakeExecutor>,
    service: Arc<RequestService>,
    repo: Arc<InMemoryRequestPersistenceRepo>,
    sink: Arc<CapturingSink>,
}

/// Build the whole chain: fake source → default projector → weight scoring →
/// pipeline(fake transformers + fake executor) → production recorder over the
/// in-memory repo. The fake inbound has no API-key profile model mapping
/// (equivalent to Go's default `APIKeyProfile.ModelMappings == nil`).
fn build_harness(
    candidates: Vec<ChannelModelsCandidate>,
    steps: Vec<Step>,
    pipe_policy: PipeRetryPolicy,
    hooks: RetryHooks,
) -> Harness {
    build_harness_with_mappings(candidates, steps, pipe_policy, hooks, Vec::new())
}

/// Same as [`build_harness`] but wires the inbound transformer with an API-key
/// profile model-mapping table (Go `APIKeyProfile.ModelMappings`). The mapping
/// runs inside the fake inbound (mirroring Go's `applyModelMapping`
/// middleware, which fires immediately after the openai inbound parses the
/// client model).
fn build_harness_with_mappings(
    candidates: Vec<ChannelModelsCandidate>,
    steps: Vec<Step>,
    pipe_policy: PipeRetryPolicy,
    hooks: RetryHooks,
    mappings: Vec<ModelMapping>,
) -> Harness {
    let source = Arc::new(FakeSource::new(candidates));
    let inbound = Arc::new(FakeInbound::with_mappings(mappings));
    let outbound = Arc::new(FakeOutbound::new());
    let executor = Arc::new(FakeExecutor::new(steps));

    let pipeline = Arc::new(
        Pipeline::new(inbound.clone(), outbound.clone(), executor.clone())
            .with_retry_policy(pipe_policy)
            .with_retry_hooks(hooks),
    );

    let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
    let service = Arc::new(RequestService::new(repo.clone()));
    let sink = Arc::new(CapturingSink::default());
    let recorder = Arc::new(ProductionRequestRecorder::new(
        service.clone(),
        sink.clone(),
    ));

    let orchestrator = CommandOrchestrator::new(
        source.clone(),
        Arc::new(DefaultCandidateProjector),
        Arc::new(WeightScoring::new()),
        Arc::new(StaticStickyKeyProvider::none()),
        LbRetryPolicy::DEFAULT,
        pipeline,
        recorder,
        Arc::new(FlagCancelToken::new()),
    );

    Harness {
        orchestrator,
        source,
        inbound,
        outbound,
        executor,
        service,
        repo,
        sink,
    }
}

/// Seed the Running request + execution rows the recorder updates (row
/// creation middlewares are a wiring gap — module docs).
async fn seed_rows(
    service: &RequestService,
    request_id: &str,
    project_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ctx = svc_ctx();
    let mut request = RequestRecord::new(request_id, "req", project_id, "POST", "/v1/chat");
    request.status = RequestStatus::Running;
    service.create_request(&ctx, request).await?;
    service
        .append_execution(
            &ctx,
            ExecutionRecord::new(format!("{request_id}-attempt-1"), request_id, project_id, 1),
        )
        .await?;
    Ok(())
}

fn no_retry_policy() -> PipeRetryPolicy {
    PipeRetryPolicy {
        enabled: false,
        max_channel_retries: 0,
        max_single_channel_retries: 0,
        retry_delay_ms: 0,
        stream_first_event_timeout_ms: 0,
        non_stream_timeout_ms: 0,
        empty_response_detection: false,
    }
}

fn chat_candidate_request(model: &str, stream: bool) -> CandidateRequest {
    CandidateRequest::new(model, RequestType::Chat, "openai/chat_completions").with_stream(stream)
}

#[derive(Clone, Copy)]
struct PreferChannelScoring(&'static str);

impl ScoringStrategy for PreferChannelScoring {
    fn name(&self) -> &'static str {
        "test-prefer-channel"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        if candidate.id == self.0 { 1_000 } else { 0 }
    }
}

#[tokio::test]
async fn api_key_profile_strategy_selects_the_matching_runtime_scorer()
-> Result<(), Box<dyn std::error::Error>> {
    let candidates = vec![
        test_candidate("1", "upstream-adaptive"),
        test_candidate("2", "upstream-failover"),
        test_candidate("3", "upstream-circuit"),
    ];
    let steps = vec![
        Step::Ok(mock_openai_response("r1", "m", "ok", 1, 1)),
        Step::Ok(mock_openai_response("r2", "m", "ok", 1, 1)),
        Step::Ok(mock_openai_response("r3", "m", "ok", 1, 1)),
    ];
    let Harness {
        orchestrator,
        outbound,
        service,
        ..
    } = build_harness(candidates, steps, no_retry_policy(), RetryHooks::default());
    let orchestrator = orchestrator.with_scoring_strategies(ScoringStrategySet::new(
        Arc::new(PreferChannelScoring("1")),
        Arc::new(PreferChannelScoring("2")),
        Arc::new(PreferChannelScoring("3")),
    ));

    for (index, profile_strategy) in ["system_default", "failover", "circuit-breaker"]
        .into_iter()
        .enumerate()
    {
        let request_id = format!("lb-profile-{index}");
        seed_rows(&service, &request_id, "project-1").await?;
        let mut request = test_request("public-model", "hello", false);
        request.metadata.insert(
            "api_key_load_balance_strategy".to_string(),
            json!(profile_strategy),
        );
        let mut ctx = OrchestratorContext::new();
        orchestrator
            .process_command(
                &mut ctx,
                &request_id,
                "project-1",
                &chat_candidate_request("public-model", false),
                request.clone(),
                &request,
                None,
                None,
            )
            .await?;
        assert_eq!(
            ctx.metadata
                .get("load_balance_strategy")
                .map(String::as_str),
            Some(match profile_strategy {
                "system_default" => "adaptive",
                other => other,
            })
        );
    }

    assert_eq!(
        outbound.seen_models(),
        vec![
            "upstream-adaptive".to_string(),
            "upstream-failover".to_string(),
            "upstream-circuit".to_string(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn association_priority_precedes_channel_ordering_weight()
-> Result<(), Box<dyn std::error::Error>> {
    let mut preferred_association = test_candidate("1", "priority-first");
    preferred_association.priority = 0;
    preferred_association.ordering_weight = 1;
    let mut heavy_lower_association = test_candidate("2", "weight-heavy");
    heavy_lower_association.priority = 1;
    heavy_lower_association.ordering_weight = 100;
    let Harness {
        orchestrator,
        outbound,
        service,
        ..
    } = build_harness(
        vec![heavy_lower_association, preferred_association],
        vec![Step::Ok(mock_openai_response("r", "m", "ok", 1, 1))],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&service, "association-priority", "project-1").await?;
    let request = test_request("public-model", "hello", false);
    orchestrator
        .process_command(
            &mut OrchestratorContext::new(),
            "association-priority",
            "project-1",
            &chat_candidate_request("public-model", false),
            request.clone(),
            &request,
            None,
            None,
        )
        .await?;

    assert_eq!(outbound.seen_models(), ["priority-first"]);
    Ok(())
}

// ===========================================================================
// A01 — fake provider full chain (also mirrors Go
// `TestChatCompletionOrchestrator_Process_NonStreaming`).
// ===========================================================================

/// A01: request → select → transform → provider → response → request/usage
/// log. Asserts each station's product. Mirrors Go
/// `TestChatCompletionOrchestrator_Process_NonStreaming`
/// (orchestrator_basic_test.go:276-377): request row Completed with external
/// id, execution row Completed, usage log with 10/20/30 tokens.
#[tokio::test]
async fn a01_full_chain_request_to_usage_log() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![Step::Ok(mock_openai_response(
            "chatcmpl-123",
            "gpt-4",
            "Hello! How can I help you?",
            10,
            20,
        ))],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&harness.service, "1", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let response = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", false),
            test_request("gpt-4", "Hello!", false),
            &test_request("gpt-4", "Hello!", false),
            None,
            None,
        )
        .await?;

    // ---- Station 1: Select — the candidate source saw the requested model.
    assert_eq!(harness.source.seen_models(), vec!["gpt-4".to_string()]);

    // ---- Station 2: stages — Select → LoadBalance → (pipeline) → Persist.
    assert_eq!(
        ctx.stages,
        vec![
            OrchestratorStage::Select,
            OrchestratorStage::LoadBalance,
            OrchestratorStage::Persist
        ]
    );
    let steps = ctx
        .metadata
        .get("pipeline_steps")
        .cloned()
        .unwrap_or_default();
    assert!(
        steps.contains("inbound:transform_request")
            && steps.contains("outbound:transform_request")
            && steps.contains("attempt:1:success"),
        "pipeline steps must record the transform/attempt chain, got: {steps}"
    );

    // ---- Station 3: transforms — inbound ran once; outbound saw the model.
    assert_eq!(harness.inbound.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.outbound.seen_models(), vec!["gpt-4".to_string()]);

    // ---- Station 4: provider — the executor got the transformed request.
    let captured = harness.executor.captured_requests();
    assert_eq!(captured.len(), 1, "executor must be called exactly once");
    assert_eq!(captured[0].path, "/v1/chat/completions");
    assert_eq!(
        captured[0]
            .json_body
            .as_ref()
            .and_then(|b| b.get("model"))
            .and_then(Value::as_str),
        Some("gpt-4")
    );

    // ---- Station 5: response — the canned provider body came back.
    assert_eq!(response.status, 200);
    assert_eq!(
        response
            .json_body
            .as_ref()
            .and_then(|b| b.get("id"))
            .and_then(Value::as_str),
        Some("chatcmpl-123")
    );

    // ---- Station 6: request row Completed with the external id (Go:
    // StatusCompleted + ExternalID "chatcmpl-123").
    let ctx_svc = svc_ctx();
    let request = harness
        .repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Succeeded);
    assert_eq!(
        request.extra.get("external_id").and_then(Value::as_str),
        Some("chatcmpl-123")
    );

    // ---- Station 7: execution row Completed with the external id.
    let execs = harness.service.list_executions(&ctx_svc, "1", "1").await?;
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].status, RequestStatus::Succeeded);
    assert_eq!(
        execs[0].extra.get("external_id").and_then(Value::as_str),
        Some("chatcmpl-123")
    );

    // ---- Station 8: usage log — Go asserts RequestID + 10/20/30 tokens.
    let rows = harness.sink.rows();
    assert_eq!(rows.len(), 1, "exactly one usage-log row expected");
    assert_eq!(rows[0].request_id, 1);
    assert_eq!(rows[0].prompt_tokens, 10);
    assert_eq!(rows[0].completion_tokens, 20);
    assert_eq!(rows[0].total_tokens, 30);
    Ok(())
}

/// A pre-resolved candidate list must be consumed without querying the source
/// again. The HTTP bridge performs inbound transform + selection once, then
/// uses this path for the execution stages.
#[tokio::test]
async fn a01_pre_resolved_candidates_avoid_duplicate_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let candidate = test_candidate("1", "gpt-4");
    let harness = build_harness(
        vec![candidate.clone()],
        vec![Step::Ok(mock_openai_response(
            "chatcmpl-pre-resolved",
            "gpt-4",
            "ok",
            1,
            1,
        ))],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&harness.service, "pre-resolved", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let response = harness
        .orchestrator
        .process_command_with_resolved_candidates(
            &mut ctx,
            harness.inbound.as_ref(),
            "pre-resolved",
            "1",
            &chat_candidate_request("gpt-4", false),
            test_request("gpt-4", "Hello!", false),
            &test_request("gpt-4", "Hello!", false),
            None,
            None,
            None,
            &[candidate],
        )
        .await?;

    assert_eq!(response.status, 200);
    assert!(harness.source.seen_models().is_empty());
    Ok(())
}

/// A01 (streaming leg): the farthest station streaming reaches through
/// `process_command` today — the pipeline's Stream arm collects the provider
/// events onto `HttpResponse.stream` (the live forward+persist loop is the
/// S38 `OutboundForwardingStream`, wired at the API layer; see A02).
#[tokio::test]
async fn a01_streaming_collects_events_through_pipeline() -> Result<(), Box<dyn std::error::Error>>
{
    let events = vec![
        StreamEvent {
            data: Some(
                r#"{"id":"chatcmpl-123","choices":[{"delta":{"content":"Hello"}}]}"#.to_string(),
            ),
            ..StreamEvent::default()
        },
        StreamEvent {
            data: Some("[DONE]".to_string()),
            ..StreamEvent::default()
        },
    ];
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![Step::Stream(events.clone())],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&harness.service, "1", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let response = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", true),
            test_request("gpt-4", "Hi!", true),
            &test_request("gpt-4", "Hi!", true),
            None,
            None,
        )
        .await?;

    // The Stream arm surfaces the provider events on `response.stream`
    // (Go returns a live stream; the Rust pipeline collects — breakpoint).
    assert_eq!(response.stream.len(), 2);
    assert_eq!(response.stream[1].data.as_deref(), Some("[DONE]"));
    let steps = ctx
        .metadata
        .get("pipeline_steps")
        .cloned()
        .unwrap_or_default();
    assert!(
        steps.contains("execute:Stream"),
        "stream mode must be taken, got: {steps}"
    );
    Ok(())
}

/// A01 (LIVE streaming leg — RUST-P8-003): `process_command_stream` drives the
/// real incremental path the buffered A01 lacked. The client-facing receiver
/// yields the provider events through the forward loop (NOT collected into a
/// `Vec` first), the terminal `[DONE]` reaches the client, and the
/// persistent-stream finalizer runs to completion (persisting the execution/
/// usage rows at stream close). This is the end-to-end proof that
/// `stream:true` → `process_command_stream` → live forward loop works.
#[tokio::test]
async fn a01_live_streaming_yields_events_via_process_command_stream()
-> Result<(), Box<dyn std::error::Error>> {
    let events = vec![
        StreamEvent {
            data: Some(
                r#"{"id":"chatcmpl-123","choices":[{"delta":{"content":"Hello"}}]}"#.to_string(),
            ),
            ..StreamEvent::default()
        },
        StreamEvent {
            data: Some("[DONE]".to_string()),
            ..StreamEvent::default()
        },
    ];
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![Step::Stream(events.clone())],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&harness.service, "1", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let mut handle = harness
        .orchestrator
        .process_command_stream(
            &mut ctx,
            harness.inbound.clone(),
            "1",
            "1",
            &chat_candidate_request("gpt-4", true),
            test_request("gpt-4", "Hi!", true),
            &test_request("gpt-4", "Hi!", true),
            None,
            None,
        )
        .await?;

    // The client-facing receiver yields the provider events (forward loop),
    // NOT a pre-collected `Vec` (the buffered A01 path).
    let mut received = Vec::new();
    while let Some(event) = handle.client_rx.recv().await {
        received.push(event);
    }
    assert!(
        !received.is_empty(),
        "live stream must yield events through client_rx"
    );
    assert!(
        received
            .iter()
            .any(|e| e.as_ref().ok().and_then(|event| event.data.as_deref()) == Some("[DONE]")),
        "the terminal [DONE] event must reach the client"
    );

    // The persistent-stream finalizer runs to completion (persists the
    // execution/usage rows on stream close).
    handle
        .finalizer
        .await
        .map_err(|e| format!("finalizer task panicked: {e}"))??;
    Ok(())
}

// ===========================================================================
// A03 — Go orchestrator_basic/error/streaming test mirrors (fake-portable
// subset; the pending list lives in the task report).
// ===========================================================================

/// Mirrors Go `TestChatCompletionOrchestrator_Process_ErrorHandling`
/// (orchestrator_error_test.go:24-101): provider 401 → Process errors and the
/// request row lands Failed. The Go assertion on the execution row's
/// error message is PENDING (record_failure does not yet receive the failed
/// attempt's execution id — module-docs breakpoint).
#[tokio::test]
async fn a03_error_handling_marks_request_failed() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![Step::Err(
            ConduitError::upstream("Invalid API key").with_provider_status(401),
        )],
        no_retry_policy(),
        RetryHooks::default(),
    );
    seed_rows(&harness.service, "1", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let result = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", false),
            test_request("gpt-4", "This will fail", false),
            &test_request("gpt-4", "This will fail", false),
            None,
            None,
        )
        .await;

    let err = match result {
        Err(err) => err,
        Ok(_) => panic!("provider failure must surface as an error"),
    };
    assert_eq!(err.failed_stage, OrchestratorStage::Pipeline);
    assert!(err.source.message.contains("Invalid API key"));

    // Request row Failed (Go: request.StatusFailed).
    let ctx_svc = svc_ctx();
    let request = harness
        .repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Failed);
    // No usage row on the failure path.
    assert!(harness.sink.rows().is_empty());
    Ok(())
}

/// Mirrors Go `TestChatCompletionOrchestrator_Process_NoChannelsAvailable`
/// (orchestrator_error_test.go:104-145): an empty selector fails the request
/// at the Select stage (Go surfaces `biz.ErrInvalidModel`; the Rust
/// orchestrator maps it to a NotFound "no candidates available" error).
#[tokio::test]
async fn a03_no_channels_available_fails_at_select() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        Vec::new(), // empty channel selector
        Vec::new(),
        no_retry_policy(),
        RetryHooks::default(),
    );

    let mut ctx = OrchestratorContext::new();
    let result = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", false),
            test_request("gpt-4", "No channels", false),
            &test_request("gpt-4", "No channels", false),
            None,
            None,
        )
        .await;

    let err = match result {
        Err(err) => err,
        Ok(_) => panic!("no candidates must surface as an error"),
    };
    assert_eq!(err.failed_stage, OrchestratorStage::Select);
    assert!(err.source.message.contains("no candidates available"));
    // Nothing reached the provider.
    assert!(harness.executor.captured_requests().is_empty());
    Ok(())
}

/// Mirrors Go `TestChatCompletionOrchestrator_Process_InvalidRequest`
/// (orchestrator_error_test.go:148-208): a request body without `model` fails
/// the inbound transform and the error mentions "model".
#[tokio::test]
async fn a03_invalid_request_missing_model() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        Vec::new(),
        no_retry_policy(),
        RetryHooks::default(),
    );
    // Seed the request row so `record_failure` can flip it to Failed (Go's
    // ent in-memory DB creates the row in `persistRequest.OnInboundLlmRequest`
    // before the inbound transform runs; without it the recorder's "request
    // not found" error masks the original "model is required" — the Go test
    // asserts on the latter).
    seed_rows(&harness.service, "1", "1").await?;

    // Go builds a raw request whose body lacks "model".
    let invalid = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        json_body: Some(json!({"messages":[{"role":"user","content":"test"}]})),
        ..HttpRequest::default()
    };

    let mut ctx = OrchestratorContext::new();
    let result = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", false),
            invalid.clone(),
            &invalid,
            None,
            None,
        )
        .await;

    let err = match result {
        Err(err) => err,
        Ok(_) => panic!("missing model must surface as an error"),
    };
    assert!(
        err.source.message.contains("model"),
        "error must mention the missing model, got: {}",
        err.source.message
    );
    assert!(harness.executor.captured_requests().is_empty());
    Ok(())
}

/// Mirrors Go `TestChatCompletionOrchestrator_Process_MultipleRequests`
/// (orchestrator_basic_test.go:733-816): three sequential requests each land
/// their own request row + usage row.
#[tokio::test]
async fn a03_multiple_requests_create_one_row_each() -> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![
            Step::Ok(mock_openai_response(
                "resp-A",
                "gpt-4",
                "Response A",
                10,
                20,
            )),
            Step::Ok(mock_openai_response(
                "resp-B",
                "gpt-4",
                "Response B",
                11,
                21,
            )),
            Step::Ok(mock_openai_response(
                "resp-C",
                "gpt-4",
                "Response C",
                12,
                22,
            )),
        ],
        no_retry_policy(),
        RetryHooks::default(),
    );

    for request_id in ["1", "2", "3"] {
        seed_rows(&harness.service, request_id, "1").await?;
        let mut ctx = OrchestratorContext::new();
        let response = harness
            .orchestrator
            .process_command(
                &mut ctx,
                request_id,
                "1",
                &chat_candidate_request("gpt-4", false),
                test_request("gpt-4", "Request", false),
                &test_request("gpt-4", "Request", false),
                None,
                None,
            )
            .await?;
        assert_eq!(response.status, 200);
    }

    // Three request rows (Go asserts 3 requests / 3 executions / 3 usage logs).
    assert_eq!(harness.repo.request_count()?, 3);
    let ctx_svc = svc_ctx();
    for request_id in ["1", "2", "3"] {
        let execs = harness
            .service
            .list_executions(&ctx_svc, "1", request_id)
            .await?;
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].status, RequestStatus::Succeeded);
    }
    assert_eq!(harness.sink.rows().len(), 3);
    Ok(())
}

/// Partial mirror of Go
/// `TestChatCompletionOrchestrator_Process_SameChannelRetryNextModel`
/// (orchestrator_basic_test.go:850-955): first provider call fails with a
/// 500, the same-channel retry succeeds, exactly two provider calls are made
/// and the request completes. PENDING vs Go: the model-index advance
/// (gpt-4 → gpt-3.5-turbo on retry) — the Rust pipeline's failover cursor is
/// wired with `model_count = 1` (single-model candidates), so the retry
/// re-sends the same model.
#[tokio::test]
async fn a03_same_channel_retry_recovers_after_upstream_error()
-> Result<(), Box<dyn std::error::Error>> {
    let harness = build_harness(
        vec![test_candidate("1", "gpt-4")],
        vec![
            Step::Err(ConduitError::upstream("upstream error").with_provider_status(500)),
            Step::Ok(mock_openai_response(
                "chatcmpl-retry-1",
                "gpt-4",
                "Recovered",
                10,
                20,
            )),
        ],
        // Go test: Enabled, MaxChannelRetries 1, MaxSingleChannelRetries 1,
        // RetryDelayMs 0.
        PipeRetryPolicy {
            enabled: true,
            max_channel_retries: 1,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            stream_first_event_timeout_ms: 0,
            non_stream_timeout_ms: 0,
            empty_response_detection: false,
        },
        // Go CanRetry: a 500 is retryable for the channel.
        RetryHooks {
            can_retry: Arc::new(|_| true),
            has_more_channels: Arc::new(|| false),
            ..RetryHooks::default()
        },
    );
    seed_rows(&harness.service, "1", "1").await?;

    let mut ctx = OrchestratorContext::new();
    let response = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &chat_candidate_request("gpt-4", false),
            test_request("gpt-4", "Hello!", false),
            &test_request("gpt-4", "Hello!", false),
            None,
            None,
        )
        .await?;

    assert_eq!(response.status, 200);
    // Two provider calls (Go: require.Len(t, executor.requests, 2)).
    assert_eq!(harness.executor.captured_requests().len(), 2);
    // One request row (Go: require.Len(t, requests, 1)) — Succeeded.
    assert_eq!(harness.repo.request_count()?, 1);
    let ctx_svc = svc_ctx();
    let request = harness
        .repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Succeeded);
    Ok(())
}

/// Mirrors Go `TestChatCompletionOrchestrator_Process_WithModelMapping`
/// (orchestrator_basic_test.go:534-637): the client sends `my-custom-model`,
/// the API-key profile carries `ModelMappings: [{From: "my-custom-model", To:
/// "gpt-4"}]`, so `applyModelMapping` rewrites the request model to `gpt-4`
/// before select + outbound. Go then asserts: provider receives `gpt-4`, the
/// request row's `ModelID` is `gpt-4`, and the response comes back.
///
/// Rust breakpoint: Rust has no ent DB / API-key profile loader wired into
/// `process_command` yet, so the mapping is applied inside the fake inbound
/// (mirroring Go's `applyModelMapping` middleware firing post-inbound). The
/// `ModelMapper::map_model_owned` decision itself is covered by the
/// `conduit-orchestrator::pre_execution` unit tests (S09); this test exercises
/// the *integration* of mapping → select → outbound → provider → row persist.
#[tokio::test]
async fn a03_with_model_mapping_rewrites_client_model_before_select()
-> Result<(), Box<dyn std::error::Error>> {
    // Go: APIKeyProfile.ModelMappings = [{From: "my-custom-model", To: "gpt-4"}].
    let mappings = vec![ModelMapping {
        from: "my-custom-model".to_string(),
        to: "gpt-4".to_string(),
    }];
    let harness = build_harness_with_mappings(
        // The channel is registered for the *mapped* model (gpt-4), matching
        // Go's `channelsToTestCandidates(..., "gpt-4")`.
        vec![test_candidate("1", "gpt-4")],
        vec![Step::Ok(mock_openai_response(
            "chatcmpl-mapped",
            "gpt-4",
            "Mapped response",
            15,
            25,
        ))],
        no_retry_policy(),
        RetryHooks::default(),
        mappings,
    );
    seed_rows(&harness.service, "1", "1").await?;

    // Go: httpRequest uses the client-facing model "my-custom-model".
    let raw = test_request("my-custom-model", "Test mapping", false);
    // The CandidateRequest is built from the *mapped* model because
    // `process_command` runs after `applyModelMapping` in the Go middleware
    // chain (mapping is a pre_execution middleware; the orchestrator entry
    // receives the post-mapping model).
    let candidate = chat_candidate_request("gpt-4", false);

    let mut ctx = OrchestratorContext::new();
    let response = harness
        .orchestrator
        .process_command(
            &mut ctx,
            "1",
            "1",
            &candidate,
            raw.clone(),
            &raw,
            None,
            None,
        )
        .await?;

    // ---- Station 1: pure-mapper decision (Go `MapModel` contract) ----
    assert_eq!(
        ModelMapper::map_model_owned(
            &[ModelMapping {
                from: "my-custom-model".to_string(),
                to: "gpt-4".to_string()
            }],
            "my-custom-model"
        ),
        "gpt-4".to_string(),
        "ModelMapper must rewrite the client model to the provider model"
    );

    // ---- Station 2: select saw the *mapped* model (gpt-4), not the client
    // label. Go: `channelsToTestCandidates(..., "gpt-4")`.
    assert_eq!(harness.source.seen_models(), vec!["gpt-4".to_string()]);

    // ---- Station 3: inbound ran once and the outbound transformer saw the
    // mapped model (Go: outbound RequestBody.Model == "gpt-4").
    assert_eq!(harness.inbound.calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.outbound.seen_models(), vec!["gpt-4".to_string()]);

    // ---- Station 4: provider received the mapped model on its body.
    let captured = harness.executor.captured_requests();
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0]
            .json_body
            .as_ref()
            .and_then(|b| b.get("model"))
            .and_then(Value::as_str),
        Some("gpt-4"),
        "provider-facing request must carry the mapped model"
    );

    // ---- Station 5: response came back (Go: require.NotNil(result)).
    assert_eq!(response.status, 200);

    // ---- Station 6: request row lands Succeeded (Go asserts the row's
    // `ModelID == "gpt-4"`; Rust's `RequestRecord` does not yet carry a
    // top-level model_id field, so we assert the row exists + completed —
    // the model-id parity is asserted by the outbound `seen_models` and the
    // executor's captured body above).
    let ctx_svc = svc_ctx();
    let request = harness
        .repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Succeeded);

    // ---- Station 7: execution row + usage log land.
    let execs = harness.service.list_executions(&ctx_svc, "1", "1").await?;
    assert_eq!(execs.len(), 1);
    assert_eq!(execs[0].status, RequestStatus::Succeeded);
    let rows = harness.sink.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prompt_tokens, 15);
    assert_eq!(rows[0].completion_tokens, 25);
    Ok(())
}

// ===========================================================================
// A02 — streaming chain with client cancel (S38 forward loop, public API).
// ===========================================================================

/// A02: streaming e2e where the client drops mid-stream. Exercises the
/// public S38/S37 surface end-to-end: `OutboundForwardingStream` +
/// `PersistentStreamFinalizer` (with execution id) + `LivePreviewObserver`
/// over the production recorder. Asserts:
/// 1. the upstream cancel token fires (Go `cancelOnCloseStream` aborting the
///    provider call),
/// 2. request row → Cancelled (Go `UpdateRequestStatusFromError` +
///    `context.Canceled`, inbound.go:157-172 / biz/request.go:1057-1058),
/// 3. execution row → Cancelled (outbound.go:160-180 / biz/request.go:791-792),
/// 4. live-preview buffers are torn down.
#[tokio::test]
async fn a02_streaming_client_cancel_cancels_upstream_and_rows()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_orchestrator::live_streaming::{LivePreviewObserver, LiveStreamRegistry};
    use conduit_orchestrator::orchestrator::{RequestRecorder, live_preview_plan};
    use conduit_orchestrator::outbound_stream::{
        OutboundForwardingStream, PersistentStreamFinalizer, StreamAggregationError,
        StreamChunkAggregator, UpstreamItem,
    };
    use conduit_pipeline::CancelToken;
    use conduit_pipeline::pipeline::{AttemptRecord, ExecutionMode};

    /// Aggregator yielding an incomplete response, so a disconnect can never
    /// be promoted to completed (Go `isCompletedAggregated == false`).
    struct IncompleteAggregator;

    #[async_trait]
    impl StreamChunkAggregator for IncompleteAggregator {
        async fn aggregate(
            &self,
            _ctx: &OrchestratorContext,
            _chunks: &[StreamEvent],
        ) -> Result<HttpResponse, StreamAggregationError> {
            let mut incomplete = HttpResponse::default();
            incomplete.json_body = Some(json!({"id":"resp_partial"}));
            Ok(incomplete)
        }
    }

    // Production recorder over the in-memory repo, rows seeded Running.
    let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
    let service = Arc::new(RequestService::new(repo.clone()));
    let sink = Arc::new(CapturingSink::default());
    let recorder: Arc<dyn RequestRecorder> = Arc::new(ProductionRequestRecorder::new(
        service.clone(),
        sink.clone(),
    ));
    seed_rows(&service, "1", "1").await?;

    // Live-preview observer mounted on the loop (S37 mount point).
    let registry = Arc::new(LiveStreamRegistry::new());
    let plan = live_preview_plan(
        true,
        true,
        true,
        Some("1".to_string()),
        Some("1-attempt-1".to_string()),
    );
    let observer = Arc::new(LivePreviewObserver::from_plan(plan, registry.clone()));

    let finalizer = PersistentStreamFinalizer::new(
        Arc::new(IncompleteAggregator),
        recorder,
        "1",
        "1",
        AttemptRecord {
            sequence: 1,
            channel_id: "1".to_string(),
            model_index: 0,
            mode: ExecutionMode::Stream,
            outcome: Ok(HttpResponse::default()),
        },
    )
    .with_execution_id("1-attempt-1");

    let (up_tx, up_rx) = tokio::sync::mpsc::channel::<UpstreamItem>(16);
    let (cl_tx, mut cl_rx) = tokio::sync::mpsc::channel::<Result<StreamEvent, ConduitError>>(16);
    let upstream_cancel = CancelToken::new();
    let forwarding =
        OutboundForwardingStream::new(up_rx, cl_tx, upstream_cancel.clone(), finalizer)
            .with_observer(observer);

    let handle = tokio::spawn(async move { forwarding.run(&OrchestratorContext::new()).await });

    // First delta flows through; live preview sees it while in flight.
    up_tx
        .send(UpstreamItem::Event(StreamEvent {
            data: Some(r#"{"choices":[{"delta":{"content":"partial"}}]}"#.to_string()),
            ..StreamEvent::default()
        }))
        .await?;
    assert!(cl_rx.recv().await.is_some());
    assert_eq!(registry.get_request_chunks("1").len(), 1);

    // Client disconnects mid-stream.
    drop(cl_rx);
    // The next provider event trips the disconnect detection.
    up_tx
        .send(UpstreamItem::Event(StreamEvent {
            data: Some(r#"{"choices":[{"delta":{"content":"never delivered"}}]}"#.to_string()),
            ..StreamEvent::default()
        }))
        .await?;

    let plan = handle.await??;

    // 1. Upstream aborted.
    assert!(upstream_cancel.is_canceled());
    // Plan resolved to the Cancelled arm.
    assert!(plan.is_canceled());
    assert_eq!(plan.final_status, RequestStatus::Cancelled);

    // 2./3. Both rows Cancelled.
    let ctx_svc = svc_ctx();
    let request = repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Cancelled);
    let execs = service.list_executions(&ctx_svc, "1", "1").await?;
    assert_eq!(execs[0].status, RequestStatus::Cancelled);

    // No usage row on the canceled path.
    assert!(sink.rows().is_empty());

    // 4. Live-preview buffers torn down (Go live wrappers' Close).
    assert!(registry.get_request_buffer("1").is_none());
    assert!(registry.get_execution_buffer("1-attempt-1").is_none());
    Ok(())
}

// ===========================================================================
// A04 — Anthropic /v1/messages non-stream vertical (inbound-threading fix).
// ===========================================================================

/// A04: an Anthropic `/v1/messages` request driven through
/// `CommandOrchestrator::process_command_with_inbound` with the
/// `AnthropicInboundTransformer` yields an Anthropic-shaped CLIENT response
/// (top-level `"type":"message"`, `"role":"assistant"`, `content` blocks,
/// `"stop_reason"`) — NOT an OpenAI chat-completion body — **even though the
/// pipeline's fixed default inbound is `OpenAiChatInbound`**.
///
/// This is the regression proof for the inbound-threading fix. Before the fix
/// the pipeline re-parsed and reshaped every request with its one fixed
/// `self.inbound` (`OpenAiChatInbound`, wired at `wiring.rs:284`), so Anthropic
/// (and every other non-OpenAI-chat) client received OpenAI-shaped bodies. By
/// building the pipeline with `OpenAiChatInbound` as the default but passing
/// `AnthropicInboundTransformer` per-request, this test proves the PASSED
/// inbound — not `self.inbound` — now drives both the inbound parse and the
/// response transform. Mirrors Go's per-format orchestrator binding
/// (`anthropic.go:45-59`).
///
/// The upstream leg is a real Anthropic round-trip: a fake executor returns an
/// Anthropic `Message` provider response which the real
/// `AnthropicOutboundTransformer::transform_response` folds into the unified
/// `LlmResponse`, which the Anthropic inbound then reshapes back into the
/// client-facing Anthropic envelope.
#[tokio::test]
async fn a04_anthropic_messages_produce_anthropic_shaped_response()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_transformers::{
        AnthropicInboundTransformer, AnthropicOutboundConfig, AnthropicOutboundTransformer,
        OpenAiChatInbound,
    };

    let model = "claude-3-5-sonnet-20241022";

    // Anthropic-shaped provider response (real upstream `Message` envelope).
    // `usage`/`metadata` mirror `mock_openai_response` so the recorder's
    // persist leg has the same inputs as A01.
    let mut provider = HttpResponse::default();
    provider.status = 200;
    provider.json_body = Some(json!({
        "id": "msg_01ABC",
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{"type": "text", "text": "Hello from Claude!"}],
        "stop_reason": "end_turn",
        "stop_sequence": Value::Null,
        "usage": {"input_tokens": 12, "output_tokens": 7}
    }));
    provider.usage = Some(Usage {
        prompt_tokens: 12,
        completion_tokens: 7,
        total_tokens: 19,
        ..Usage::default()
    });
    provider
        .metadata
        .insert("llm_response_id".to_string(), json!("msg_01ABC"));
    provider
        .metadata
        .insert("latency_ms".to_string(), json!(90));

    // Build the orchestrator by hand. The pipeline's FIXED default inbound is
    // `OpenAiChatInbound` (the pre-fix bug source); the outbound is the real
    // Anthropic outbound; the executor replays the canned Anthropic response.
    let source = Arc::new(FakeSource::new(vec![test_candidate("1", model)]));
    let outbound = Arc::new(AnthropicOutboundTransformer::new(
        AnthropicOutboundConfig::new("https://api.anthropic.com/v1", "sk-test"),
    ));
    let executor = Arc::new(FakeExecutor::new(vec![Step::Ok(provider)]));
    let pipeline = Arc::new(
        Pipeline::new(Arc::new(OpenAiChatInbound::new()), outbound, executor)
            .with_retry_policy(no_retry_policy())
            .with_retry_hooks(RetryHooks::default()),
    );

    let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
    let service = Arc::new(RequestService::new(repo.clone()));
    let sink = Arc::new(CapturingSink::default());
    let recorder = Arc::new(ProductionRequestRecorder::new(
        service.clone(),
        sink.clone(),
    ));
    let orchestrator = CommandOrchestrator::new(
        source,
        Arc::new(DefaultCandidateProjector),
        Arc::new(WeightScoring::new()),
        Arc::new(StaticStickyKeyProvider::none()),
        LbRetryPolicy::DEFAULT,
        pipeline,
        recorder,
        Arc::new(FlagCancelToken::new()),
    );
    seed_rows(&service, "1", "1").await?;

    // Anthropic `/v1/messages` client request. `max_tokens` is REQUIRED by the
    // Anthropic inbound validator (Go inbound.go:63).
    let anthropic_request = HttpRequest {
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        json_body: Some(json!({
            "model": model,
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hi Claude"}],
        })),
        ..HttpRequest::default()
    };

    // The per-request inbound: Anthropic (NOT the pipeline's OpenAiChatInbound).
    let inbound = AnthropicInboundTransformer::new();
    let mut ctx = OrchestratorContext::new();
    let response = orchestrator
        .process_command_with_inbound(
            &mut ctx,
            &inbound,
            "1",
            "1",
            &chat_candidate_request(model, false),
            anthropic_request.clone(),
            &anthropic_request,
            None,
            None,
            None,
        )
        .await?;

    // ---- The CLIENT response is Anthropic-shaped, NOT OpenAI-shaped. ----
    assert_eq!(response.status, 200);
    let body = response
        .json_body
        .as_ref()
        .ok_or("client response must carry a JSON body")?;

    assert_eq!(
        body.get("type").and_then(Value::as_str),
        Some("message"),
        "top-level type must be the Anthropic \"message\", got: {body}"
    );
    assert_eq!(body.get("role").and_then(Value::as_str), Some("assistant"));
    assert_eq!(
        body.get("stop_reason").and_then(Value::as_str),
        Some("end_turn"),
        "finish_reason stop must map to Anthropic stop_reason end_turn"
    );
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or("Anthropic response content must be an array of blocks")?;
    assert_eq!(content.len(), 1, "one text content block expected");
    assert_eq!(content[0].get("type").and_then(Value::as_str), Some("text"));
    assert_eq!(
        content[0].get("text").and_then(Value::as_str),
        Some("Hello from Claude!")
    );

    // ---- Explicitly NOT the OpenAI chat.completion shape. ----
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("choices")),
        "Anthropic client response must not carry OpenAI `choices`, got: {body}"
    );
    assert_ne!(
        body.get("object").and_then(Value::as_str),
        Some("chat.completion"),
        "Anthropic client response must not be an OpenAI chat.completion object"
    );

    // ---- The request row lands Succeeded (persist leg still runs). ----
    let ctx_svc = svc_ctx();
    let request = repo
        .find_request(&ctx_svc, "1", "1")
        .await?
        .ok_or("request row must exist")?;
    assert_eq!(request.status, RequestStatus::Succeeded);
    Ok(())
}

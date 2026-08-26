#![forbid(unsafe_code)]

pub mod cancel;
pub mod empty_response;
pub mod error_rewrite;
pub mod middleware;
pub mod pass_through;
pub mod pipeline;
pub mod retryable;
pub mod upstream_error;

pub use middleware::{
    BoxEventStream, BoxLlmStream, BoxPipelineMiddleware, PipelineContext, PipelineMiddleware,
    PipelineResult, apply_before_request_middlewares, apply_inbound_raw_response_middlewares,
    apply_inbound_raw_stream_middlewares, apply_llm_response_middlewares,
    apply_llm_stream_middlewares, apply_raw_error_response_middlewares,
    apply_raw_request_middlewares, apply_raw_response_middlewares, apply_raw_stream_middlewares,
};
pub use pipeline::{
    AttemptError, AttemptObservation, AttemptObservationOutcome, AttemptObserver, AttemptOutcome,
    AttemptRecord, AttemptStage, CanRetryFn, CustomizeExecutorFn, ExecutionMode, Executor,
    FailoverError, HasMoreChannelsFn, IsTimeoutErrorFn, MAX_RETRY_RESPONSE_TIMEOUT_SECONDS,
    NON_STREAM_RESPONSE_TIMEOUT_CODE, Pipeline, PipelineCandidate, RetryContext, RetryDecision,
    RetryHooks, RetryPolicy, RetryState, STREAM_FIRST_EVENT_TIMEOUT_CODE, StreamMode,
    clamp_response_timeout_seconds, decide_retry, decide_stream_mode,
    is_non_stream_response_timeout, is_response_timeout_error, is_stream_first_event_timeout,
    non_stream_response_timeout_error, stream_first_event_timeout_error,
};

pub const CRATE_NAME: &str = "conduit-pipeline";

pub use cancel::{CancelGuard, CancelOnCloseStream, CancelToken};

pub use empty_response::{
    ErrEmptyAggregatedBody, ErrEmptyResponse, ErrEmptyStreamChunks, ErrNonStreamResponseTimeout,
    ErrStreamFirstEventTimeout, MAX_PRE_READ_EVENTS, PreReadOutcome, has_finish_reason,
    has_message_content, has_response_content, pre_read_llm_stream,
};
pub use error_rewrite::apply_error_response_rewrite;

pub use retryable::{
    channel_retry_hook, extract_status_code_from_error, is_channel_extra_retryable,
    is_http_status_code_retryable, is_rate_limit_error, is_retryable_error,
    is_retryable_error_for_channel, matches_retryable_error_pattern,
};

pub use upstream_error::{
    DEFAULT_UPSTREAM_ERROR_MESSAGE, UPSTREAM_ERROR_MARKER, apply_upstream_error_policy,
    is_upstream_error, wrap_upstream_error,
};

// RUST-P8-003 — pipeline-side pass-through seams (S04-S11). Pure helpers are
// re-exported; the trait seams (PassThroughRecorder) and callback alias
// (FormatStreamErrorFn) are exposed for the orchestrator to wire.
pub use pass_through::{
    FormatStreamErrorFn, PassThroughAttempt, PassThroughDecision, PassThroughRecorder,
    filter_pass_through_headers, format_stream_error_with, merge_pass_through_request_body,
    pass_through_body_needs_model_patch, pass_through_body_supported, pass_through_stream_aligned,
};

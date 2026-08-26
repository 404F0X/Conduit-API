#![forbid(unsafe_code)]

pub mod bridge;
pub mod candidate;
pub mod candidates;
pub mod channel_limiter;
// Orphan-file fix: these modules exist on disk and are used by the host
// wiring (conduit-bin) but were never declared here, so they were never
// compiled. Declaring them brings the DB-backed candidate source and the
// reqwest-backed upstream executor into the crate.
pub mod db_candidate_source;
pub mod errors;
pub mod live_streaming;
pub mod load_balancer;
pub mod middlewares;
pub mod openai_bridge;
pub mod orchestrator;
pub mod outbound_stream;
pub mod pass_through;
pub mod pre_execution;
pub mod quota;
pub mod rate_limit;
pub mod request_recorder;
pub mod upstream_executor;

pub const CRATE_NAME: &str = "conduit-orchestrator";

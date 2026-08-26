#![forbid(unsafe_code)]

//! Lightweight compatibility-test helpers for the Conduit API Rust migration.
//!
//! This crate intentionally avoids runtime-heavy dependencies. It provides
//! reusable fixture shapes, fake upstream response plans, stream parsing,
//! PostgreSQL comparison labels, and golden comparison helpers.

pub mod db;
pub mod fake_provider;
pub mod fixtures;
pub mod golden;
pub mod new_api_gateway;
pub mod stream;

pub use db::{DATABASE_BACKEND, DbComparisonKind, DbTestArea};
pub use fake_provider::{
    FakeProvider, FakeProviderBuilder, FakeResponse, FakeRoute, RecordedRequest,
};
pub use fixtures::{BodyFixture, HeaderFixture, HttpFixture};
pub use golden::{
    ALLOWED_PLACEHOLDERS, GoldenCase, GoldenSseEvent, HeaderAssertion, HeaderComparison,
    JsonComparisonOptions, compare_headers, compare_json, parse_golden_case,
};
pub use new_api_gateway::{MockGatewayConfig, MockGatewayServer, MockRequestRecord};
pub use stream::{
    SseEvent, StreamComparison, compare_golden_sse_events, compare_golden_sse_text, parse_sse,
};

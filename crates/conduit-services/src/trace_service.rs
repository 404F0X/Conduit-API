//! Trace service — Rust port of `conduit/internal/server/biz/trace.go` plus
//! the pure header/body extraction logic from
//! `conduit/internal/server/middleware/trace.go`.
//!
//! Mirrors the Go `TraceService` data-access surface (`GetOrCreateTrace` /
//! `GetTraceByID`) at the service layer: atomic get-or-create by
//! `(project_id, trace_id)` and get-by-pair. The Go service queries by
//! `(trace_id, project_id)` and, on a create-time constraint violation,
//! re-queries by the same pair; `get_or_create_trace` collapses this into a
//! first-write-wins check keyed on the pair.
//!
//! ## thread_id linkage (mirrors Go semantics)
//! `thread_id` is optional and **immutable** — it is only applied on the
//! create path (Go uses `SetNillableThreadID`; the `thread_id` field is marked
//! `Immutable` in the Ent schema). A second `get_or_create_trace` for an
//! existing pair keeps the original `thread_id`, even if a different value is
//! supplied. When supplied, `thread_id` is a free-form reference to a thread
//! in the same project (the trace and thread are created independently; the
//! service does not validate the thread's existence here).
//!
//! ## Project isolation
//! Same `trace_id` in different projects are independent rows — the pair
//! `(project_id, trace_id)` is the logical key. `get_trace` is project-scoped.
//!
//! Traces have no soft delete; there is no `*_with_deleted` surface here.
//!
//! ## Header / body extraction (mirrors `middleware/trace.go`)
//! `resolve_trace_id` runs the Go priority chain purely over a header map and
//! an optional JSON body, gated by the same `*TraceEnabled` flags. The
//! middleware wiring (reading `c.Request.Body`, restoring it, system bypass)
//! lives in the HTTP layer; this module only owns the testable decision.
//!
//! Quoting the Go priority chain (`middleware/trace.go` WithTrace):
//!   1. primary trace header (default `Conduit-Trace-Id`) — `getTraceIDFromHeader`
//!   2. `config.ExtraTraceHeaders` — first non-empty wins
//!   3. OpenCode `x-session-affinity` when `OpenCodeTraceEnabled`
//!   4. Claude Code `metadata.user_id` when `ClaudeCodeTraceEnabled`
//!   5. Codex session header / turn metadata when `CodexTraceEnabled`
//!   6. `config.ExtraTraceBodyFields` — gjson-style dotted paths into the body

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub type TraceServiceResult<T> = Result<T, TraceServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TraceServiceError {
    #[error("trace persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRecord {
    pub id: String,
    pub project_id: String,
    pub external_id: String,
    pub thread_id: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl TraceRecord {
    pub fn new(
        project_id: impl Into<String>,
        external_id: impl Into<String>,
        thread_id: Option<String>,
    ) -> Self {
        let project_id = project_id.into();
        let external_id = external_id.into();
        Self {
            id: scoped_record_id("trace", &project_id, &external_id),
            project_id,
            external_id,
            thread_id,
            created_at: Utc::now(),
            extra: BTreeMap::new(),
        }
    }
}

#[async_trait]
pub trait TraceServiceRepo: Send + Sync {
    /// Atomic get-or-create by `(project_id, external_id)`. Returns the
    /// existing record when the pair already exists; otherwise inserts and
    /// returns a new record. Mirrors Go `TraceService.GetOrCreateTrace`.
    ///
    /// `thread_id` is only applied on the create path — an existing record
    /// keeps its original value (the Go schema marks `thread_id` `Immutable`).
    async fn get_or_create_trace(
        &self,
        ctx: &RequestContext,
        record: TraceRecord,
    ) -> TraceServiceResult<TraceRecord>;

    /// Get an existing trace by `(project_id, external_id)`. Returns `None`
    /// when no record matches. Mirrors Go `TraceService.GetTraceByID`
    /// (the Go path surfaces "not found" as an error; the service exposes it
    /// as `Option` for caller convenience).
    async fn find_trace(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> TraceServiceResult<Option<TraceRecord>>;
}

pub struct TraceService {
    repo: Arc<dyn TraceServiceRepo>,
}

impl TraceService {
    pub fn new(repo: Arc<dyn TraceServiceRepo>) -> Self {
        Self { repo }
    }

    /// Get-or-create a trace scoped by `(project_id, external_id)`. Idempotent
    /// for the same pair; cross-project the same `external_id` yields distinct
    /// records. `thread_id`, when supplied, links the trace to a thread — it
    /// is applied only on the first create (immutable post-create, mirroring
    /// Go `SetNillableThreadID`). Mirrors Go `TraceService.GetOrCreateTrace`.
    pub async fn get_or_create_trace(
        &self,
        ctx: &RequestContext,
        project_id: impl Into<String>,
        external_id: impl Into<String>,
        thread_id: Option<String>,
    ) -> TraceServiceResult<TraceRecord> {
        self.repo
            .get_or_create_trace(ctx, TraceRecord::new(project_id, external_id, thread_id))
            .await
    }

    /// Look up a trace by `(project_id, external_id)`. Returns `None` when no
    /// such trace exists. Project-scoped — an `external_id` in another project
    /// is not visible here. Mirrors Go `TraceService.GetTraceByID`.
    pub async fn get_trace(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> TraceServiceResult<Option<TraceRecord>> {
        self.repo.find_trace(ctx, project_id, external_id).await
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryTraceServiceRepo {
    inner: Arc<Mutex<BTreeMap<(String, String), TraceRecord>>>,
}

impl InMemoryTraceServiceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn trace_count(&self) -> TraceServiceResult<usize> {
        Ok(self.lock()?.len())
    }

    fn lock(
        &self,
    ) -> TraceServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), TraceRecord>>>
    {
        self.inner
            .lock()
            .map_err(|_| TraceServiceError::LockPoisoned)
    }
}

#[async_trait]
impl TraceServiceRepo for InMemoryTraceServiceRepo {
    async fn get_or_create_trace(
        &self,
        _ctx: &RequestContext,
        record: TraceRecord,
    ) -> TraceServiceResult<TraceRecord> {
        let mut inner = self.lock()?;
        let key = (record.project_id.clone(), record.external_id.clone());
        // The first write owns optional thread linkage for this trace external id;
        // later calls for the same pair return the original record unchanged
        // (thread_id is immutable post-create — mirrors Go SetNillableThreadID).
        Ok(inner.entry(key).or_insert(record).clone())
    }

    async fn find_trace(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> TraceServiceResult<Option<TraceRecord>> {
        Ok(self
            .lock()?
            .get(&(project_id.into(), external_id.into()))
            .cloned())
    }
}

fn scoped_record_id(kind: &str, project_id: &str, external_id: &str) -> String {
    format!("{kind}:{project_id}:{external_id}")
}

// =========================================================================
// Pure header / body extraction — mirrors `middleware/trace.go` (S04–S09, S14)
// =========================================================================

/// Default trace header when `TracingConfig::trace_header` is empty.
/// Go: `middleware/trace.go` `traceHeaderName` returns `"Conduit-Trace-Id"`.
pub const DEFAULT_TRACE_HEADER: &str = "Conduit-Trace-Id";

/// OpenCode session-affinity header — Go `tryExtractTraceIDFromOpenCodeRequest`
/// reads `x-session-affinity`.
pub const OPENCODE_SESSION_AFFINITY_HEADER: &str = "x-session-affinity";

/// Codex header constants — Go `llm/transformer/openai/codex/headers.go`.
pub const CODEX_SESSION_HEADER: &str = "Session_id";
pub const CODEX_TURN_METADATA_HEADER: &str = "X-Codex-Turn-Metadata";

/// Anthropic Messages API paths — Go `tryExtractTraceIDFromClaudeCodeRequest`
/// only triggers on POST to `/anthropic/v1/messages` or `/v1/messages`.
pub const CLAUDE_CODE_MESSAGES_PATHS: &[&str] = &["/anthropic/v1/messages", "/v1/messages"];

/// Mirrors Go `internal/tracing.Config` (tracing.go) — only the fields the
/// extraction logic reads. Defaults match the Go zero-value behavior: empty
/// header names defer to `DEFAULT_TRACE_HEADER` / `DEFAULT_THREAD_HEADER`,
/// and all `*_enabled` flags default to `false` (S14 disable switch).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TracingConfig {
    /// Override for the primary trace header name. Empty → `DEFAULT_TRACE_HEADER`.
    pub trace_header: String,
    /// Override for the primary thread header name. Empty → `DEFAULT_THREAD_HEADER`.
    pub thread_header: String,
    /// Extra header names probed (in order) when the primary is absent.
    pub extra_trace_headers: Vec<String>,
    /// Dotted JSON paths probed (in order) in the request body when all
    /// header sources miss. e.g. `["trace_id", "metadata.trace_id"]`.
    pub extra_trace_body_fields: Vec<String>,
    pub claude_code_trace_enabled: bool,
    pub codex_trace_enabled: bool,
    pub opencode_trace_enabled: bool,
}

impl TracingConfig {
    /// Effective trace header name — Go `traceHeaderName(config)`.
    pub fn effective_trace_header(&self) -> &str {
        if self.trace_header.is_empty() {
            DEFAULT_TRACE_HEADER
        } else {
            &self.trace_header
        }
    }

    /// Effective thread header name — Go `WithThread` resolves the same way.
    pub fn effective_thread_header(&self) -> &str {
        if self.thread_header.is_empty() {
            crate::DEFAULT_THREAD_HEADER
        } else {
            &self.thread_header
        }
    }
}

/// Header lookup that mirrors Go's `http.Header.Get` semantics: HTTP header
/// keys are case-insensitive, so the comparison folds both sides to lowercase.
/// Go's `gin.Context.GetHeader` ultimately calls `http.Header.Get`, which
/// canonicalizes the key; we accept any case from the caller.
pub fn header_get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let name_lower = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_ascii_lowercase() == name_lower)
        .map(|(_, v)| v.as_str())
}

/// S05 — extract the trace id from the primary trace header, falling back to
/// `extra_trace_headers` in declaration order. Pure port of Go
/// `getTraceIDFromHeader` (the first half of the chain).
///
/// Returns the first non-empty (after trim) value, or `None` when no
/// configured header is present.
pub fn extract_trace_id_from_headers(
    headers: &[(String, String)],
    config: &TracingConfig,
) -> Option<String> {
    // Go falls through to extra headers when the primary is absent *or* empty.
    if let Some(primary) = header_get(headers, config.effective_trace_header()) {
        let trimmed = primary.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }

    for extra in &config.extra_trace_headers {
        if let Some(v) = header_get(headers, extra) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_owned());
            }
        }
    }
    None
}

/// S09 — OpenCode extraction. Mirrors Go
/// `tryExtractTraceIDFromOpenCodeRequest` which reads `x-session-affinity`.
/// Pure: the caller gates this behind `opencode_trace_enabled`.
pub fn extract_trace_id_from_opencode(headers: &[(String, String)]) -> Option<String> {
    let v = header_get(headers, OPENCODE_SESSION_AFFINITY_HEADER)?;
    let v = v.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_owned())
    }
}

/// S07 — Codex extraction. Mirrors Go
/// `llm/transformer/openai/codex.GetSessionIDFromHeaders`:
/// `Session_id` header first, then `X-Codex-Turn-Metadata` JSON
/// `session_id`. Pure: the caller gates this behind `codex_trace_enabled`.
pub fn extract_trace_id_from_codex(headers: &[(String, String)]) -> Option<String> {
    if let Some(v) = header_get(headers, CODEX_SESSION_HEADER) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_owned());
        }
    }

    let raw = header_get(headers, CODEX_TURN_METADATA_HEADER)?;
    extract_codex_turn_metadata_session_id(raw)
}

/// Parse `X-Codex-Turn-Metadata` JSON `{ "session_id": "..." }` and return the
/// trimmed session id. Mirrors Go `codex.ExtractSessionIDFromTurnMetadata`.
/// Empty/invalid/missing `session_id` → `None`.
pub fn extract_codex_turn_metadata_session_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let session = value.get("session_id")?.as_str()?;
    let session = session.trim();
    if session.is_empty() {
        None
    } else {
        Some(session.to_owned())
    }
}

/// S07 — Claude Code extraction. Mirrors Go
/// `tryExtractTraceIDFromClaudeCodeRequest`: only fires for POST requests to
/// the Messages API, then reads `metadata.user_id` from the JSON body and
/// parses it via `claudecode.ParseUserID`. Returns the parsed `session_id`.
///
/// `method` should be uppercase (e.g. `"POST"`); `path` is the request URL
/// path. `body_json` is the parsed request body (or `None` if the body could
/// not be parsed as JSON — Go logs and treats this as no extraction).
pub fn extract_trace_id_from_claude_code(
    method: &str,
    path: &str,
    body_json: Option<&serde_json::Value>,
) -> Option<String> {
    if method != "POST" {
        return None;
    }
    if !CLAUDE_CODE_MESSAGES_PATHS.contains(&path) {
        return None;
    }

    let body = body_json?;
    let user_id = body.get("metadata")?.get("user_id")?.as_str()?;
    let uid = parse_claude_code_user_id(user_id)?;
    let session = uid.session_id.trim();
    if session.is_empty() {
        None
    } else {
        Some(session.to_owned())
    }
}

/// Parsed Claude Code user_id — mirrors Go `claudecode.UserID`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClaudeCodeUserId {
    pub device_id: String,
    pub account_uuid: String,
    pub session_id: String,
}

/// Legacy Claude Code user_id pattern — Go `claudecode.legacyPattern`.
///
/// Kept as a private helper that builds the regex on each call. This path is
/// only reached when a request body actually contains a Claude Code legacy
/// `user_id`, so the per-call cost is negligible and avoids any `OnceLock`
/// + `unwrap_or_else` chain (workspace forbids `unwrap`/`expect`).
///
/// Returns `None` only if the static literal somehow fails to compile — which
/// cannot happen for this known-good pattern; callers propagate `None` as
/// "no legacy match".
fn legacy_claude_pattern() -> Option<regex::Regex> {
    regex::Regex::new(
        r"^user_([a-fA-F0-9]{64})_account__session_([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$",
    )
    .ok()
}

/// Parse a Claude Code user_id (legacy or v2 JSON). Mirrors Go
/// `claudecode.ParseUserID` — returns `None` for any input that does not
/// match either format, or that matches but has an empty `session_id`.
///
/// - Legacy: `user_<64hex>_account__session_<uuid-v4>`
/// - V2 JSON (>=2.1.78): `{"device_id":"...","account_uuid":"...","session_id":"..."}`
pub fn parse_claude_code_user_id(raw: &str) -> Option<ClaudeCodeUserId> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Try v2 JSON format first — Go: `strings.HasPrefix(raw, "{")`.
    if raw.starts_with('{') {
        let uid: ClaudeCodeUserId = serde_json::from_str(raw).ok()?;
        if uid.session_id.is_empty() {
            return None;
        }
        return Some(uid);
    }

    // Legacy format.
    let re = legacy_claude_pattern()?;
    let caps = re.captures(raw)?;
    Some(ClaudeCodeUserId {
        device_id: caps.get(1)?.as_str().to_owned(),
        account_uuid: String::new(),
        session_id: caps.get(2)?.as_str().to_owned(),
    })
}

/// Resolve a trace id from the body using `extra_trace_body_fields` — Go's
/// `tryGetTraceIDFromBody`. Walks the dotted paths in order and returns the
/// first non-empty string value at that path.
pub fn extract_trace_id_from_body(body: &serde_json::Value, fields: &[String]) -> Option<String> {
    for field in fields {
        if let Some(v) = resolve_dotted_path(body, field)
            && let Some(s) = v.as_str()
        {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    None
}

/// Walk a dotted gjson-style path (`metadata.trace_id`) through a JSON value.
/// Mirrors `gjson.GetBytes(body, field)` for the subset we need: nested
/// object key descent only (no array wildcards / `#(...)` queries — Go's
/// configured paths are plain dotted keys).
fn resolve_dotted_path<'a>(
    mut value: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        value = value.get(segment)?;
    }
    Some(value)
}

/// Where a resolved trace id came from — mirrors the Go `log.Debug` calls in
/// each `tryExtract...` helper. Used by S06 callers for observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSource {
    PrimaryHeader,
    ExtraHeader,
    OpenCode,
    ClaudeCode,
    Codex,
    ExtraBody,
}

/// S06 — full extraction result. `enabled` reflects whether the resolved
/// source was actually enabled in `TracingConfig` (the disable switch S14
/// makes `enabled=false` even when a header is present, so callers can still
/// surface it in logs without creating a trace row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceExtraction {
    pub enabled: bool,
    pub trace_id: Option<String>,
    pub source: Option<TraceSource>,
}

impl TraceExtraction {
    /// No trace id resolved anywhere.
    pub fn none() -> Self {
        Self {
            enabled: false,
            trace_id: None,
            source: None,
        }
    }
}

/// S06 — run the full Go priority chain purely over headers + optional body.
/// Mirrors `middleware/trace.go::WithTrace` lines 78-107 exactly:
///
/// 1. `extract_trace_id_from_headers` (primary + extra headers)
/// 2. OpenCode `x-session-affinity` when `opencode_trace_enabled`
/// 3. Claude Code `metadata.user_id` when `claude_code_trace_enabled`
/// 4. Codex `Session_id` / turn metadata when `codex_trace_enabled`
/// 5. `extra_trace_body_fields` against `body_json`
///
/// `method`/`path` are used only by the Claude Code branch (POST + Messages
/// path guard). `body_json` is `None` when there is no body or it failed to
/// parse as JSON.
///
/// **S14 disable switch**: when a source's flag is off but its header is
/// present, this returns `enabled=false` with the trace id and source
/// populated — so the middleware can still log the id without creating a row,
/// matching the Go behavior where each branch is simply skipped.
///
/// In practice (to match Go exactly) the primary/extra-header branches always
/// run regardless of any enable flag (they're the "explicit opt-in" path);
/// the OpenCode/Claude/Codex branches are gated by their respective flags,
/// and the body-fields branch runs whenever `extra_trace_body_fields` is
/// non-empty.
pub fn resolve_trace_id(
    headers: &[(String, String)],
    body_json: Option<&serde_json::Value>,
    method: &str,
    path: &str,
    config: &TracingConfig,
) -> TraceExtraction {
    // 1. Primary + extra headers — Go always runs these.
    if let Some(id) = extract_trace_id_from_headers(headers, config) {
        // Distinguish primary vs extra for observability.
        let from_primary = header_get(headers, config.effective_trace_header())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let source = if from_primary {
            TraceSource::PrimaryHeader
        } else {
            TraceSource::ExtraHeader
        };
        return TraceExtraction {
            enabled: true,
            trace_id: Some(id),
            source: Some(source),
        };
    }

    // 2. OpenCode — gated by `opencode_trace_enabled`.
    if let Some(id) = extract_trace_id_from_opencode(headers) {
        return TraceExtraction {
            enabled: config.opencode_trace_enabled,
            trace_id: Some(id),
            source: Some(TraceSource::OpenCode),
        };
    }

    // 3. Claude Code — gated by `claude_code_trace_enabled`.
    if let Some(id) = extract_trace_id_from_claude_code(method, path, body_json) {
        return TraceExtraction {
            enabled: config.claude_code_trace_enabled,
            trace_id: Some(id),
            source: Some(TraceSource::ClaudeCode),
        };
    }

    // 4. Codex — gated by `codex_trace_enabled`.
    if let Some(id) = extract_trace_id_from_codex(headers) {
        return TraceExtraction {
            enabled: config.codex_trace_enabled,
            trace_id: Some(id),
            source: Some(TraceSource::Codex),
        };
    }

    // 5. Extra body fields — gated by configuration (non-empty list).
    if let Some(body) = body_json
        && let Some(id) = extract_trace_id_from_body(body, &config.extra_trace_body_fields)
    {
        return TraceExtraction {
            enabled: true,
            trace_id: Some(id),
            source: Some(TraceSource::ExtraBody),
        };
    }

    TraceExtraction::none()
}

/// S14 — should the middleware actually persist a trace/thread record?
/// Mirrors the implicit Go behavior: the disable switch is per-source, and
/// the primary/extra-header path is always enabled (you opted in by setting
/// the header). When `extraction.enabled` is false, the middleware must NOT
/// call `get_or_create_*`, but the resolved trace id may still be logged.
pub fn should_record_trace(extraction: &TraceExtraction) -> bool {
    extraction.enabled && extraction.trace_id.is_some()
}

// =========================================================================
// get-or-create decision (S10/S11) — pure, no I/O
// =========================================================================

/// Pure get-or-create decision — given an existing row lookup result, decide
/// whether to create a new record or reuse the existing one. Mirrors Go
/// `TraceService.GetOrCreateTrace` decision shape (the I/O is the caller's
/// job). The same shape applies to threads (Go `GetOrCreateThread`).
///
/// `existing` is what the lookup-by-`(project_id, external_id)` returned:
/// - `Some(row)` → reuse it (Go: `err == nil` path, or the post-constraint
///   re-query path).
/// - `None`      → create a new record with the given `external_id`.
///
/// `thread_id` (trace only) is applied solely on the create branch — the
/// Go schema marks `thread_id` `Immutable`, so a reuse keeps the original.
//
// `Eq` is intentionally omitted: `TraceRecord` carries a `BTreeMap<String,
// Value>` and `serde_json::Value` is not `Eq` (NaN-equivalent floats).
#[derive(Debug, Clone, PartialEq)]
pub enum GetOrCreateDecision {
    /// Reuse this existing record. `thread_id` is whatever the row already
    /// stores (any value supplied on this call is ignored, mirroring Go's
    /// `Immutable` schema field).
    Reuse { existing: TraceRecord },
    /// Persist a fresh record. `record` carries the create-time `thread_id`.
    Create { record: TraceRecord },
}

/// S10/S11 — decide get-or-create for the trace path. Pure; the caller owns
/// the actual `find_trace` lookup and the repo write.
pub fn decide_trace_get_or_create(
    existing: Option<TraceRecord>,
    project_id: impl Into<String>,
    external_id: impl Into<String>,
    thread_id: Option<String>,
) -> GetOrCreateDecision {
    match existing {
        Some(row) => GetOrCreateDecision::Reuse { existing: row },
        None => GetOrCreateDecision::Create {
            record: TraceRecord::new(project_id, external_id, thread_id),
        },
    }
}

// =========================================================================
// S15 — sticky-channel (sticky-key) selection. Pure.
//
// Mirrors the pure *decision* shape of Go `TraceStickyKeyProvider.Get`
// (`internal/server/biz/channel_apikey_provider.go`) and its underlying
// `rendezvousSelect` (Highest Random Weight / Rendezvous hashing).
//
// `TraceStickyKeyProvider` is the closest Go analogue to a "sticky channel"
// selection: it pins an API key deterministically per trace so that all
// requests in the same trace hit the same upstream credential (which in
// practice means the same backing channel/pool member). The Go provider
// combines:
//   (a) a per-trace LRU cache of previous selections (stateful — caller-owned),
//   (b) a deterministic rendezvous hash over the *currently-enabled* key set,
//   (c) a fallback to the first configured key when no keys are enabled,
//   (d) a random pick when there is no trace in context.
//
// This module owns only the **pure** pieces that can be unit-tested without
// I/O or mutable state:
//   * `prefer_sticky_channel` — the sticky-vs-fallback decision given a prior
//     selection and the current candidate set (cases a/c above).
//   * `rendezvous_select` — the deterministic hash pick (case b), ported
//     verbatim from Go `rendezvousSelect` + `hashAPIKey` (FNV-1a 64-bit).
// The LRU cache and the random no-trace pick are intentionally left to the
// caller — they are not pure and are not part of this extraction target.
//
// Go source of truth (quote, `channel_apikey_provider.go`):
//   ```go
//   func (p *TraceStickyKeyProvider) Get(ctx context.Context) string {
//       enabled := p.channel.cachedEnabledAPIKeys
//       if len(enabled) == 0 {
//           return p.channel.Credentials.APIKeys[0]        // (c) fallback
//       }
//       if len(enabled) == 1 {
//           return enabled[0]
//       }
//       ...
//       if cached, ok := p.cache.Get(trace.TraceID); ok {
//           selectedKey = cached                              // (a) sticky reuse
//       } else {
//           selectedKey = rendezvousSelect(enabled, trace.TraceID)
//           p.cache.Add(trace.TraceID, selectedKey)
//       }
//       ...
//   }
//
//   func rendezvousSelect(keys []string, seed string) string {
//       bestKey := keys[0]
//       bestScore := hashAPIKey(seed + "|" + bestKey)
//       for i := 1; i < len(keys); i++ {
//           k := keys[i]
//           s := hashAPIKey(seed + "|" + k)
//           if s > bestScore { bestScore = s; bestKey = k }
//       }
//       return bestKey
//   }
//
//   func hashAPIKey(s string) uint64 {
//       h := fnv.New64a()
//       _, _ = h.Write([]byte(s))
//       return h.Sum64()
//   }
//   ```
// =========================================================================

/// Outcome of a sticky-channel decision. Mirrors the two branches of Go
/// `TraceStickyKeyProvider.Get`: either the previously-selected channel/key is
/// still valid (sticky reuse), or we must fall back to a (re)computed choice.
///
/// `selected` is always the key the caller should actually use — for `Use` it
/// equals the historical choice; for `Fallback` it is the freshly-computed
/// selection the caller should also persist into its sticky cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StickyDecision {
    /// The historical sticky selection is still in the candidate set — keep it.
    Use { selected: String },
    /// The historical selection is no longer available (or there is no prior
    /// selection). `reason` records why we fell back; `selected` is the new
    /// deterministic pick the caller should pin going forward.
    Fallback {
        selected: String,
        reason: StickyFallbackReason,
    },
}

/// Why a sticky selection could not be honored. Mirrors the Go branches:
/// `NoHistory` = no cached selection (cold path), `RemovedFromCandidates` =
/// the key was disabled/removed from the channel, `NoCandidates` = the
/// enabled-key set is empty (Go falls back to `Credentials.APIKeys[0]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyFallbackReason {
    /// No prior selection was supplied (Go: trace cache miss on first call).
    NoHistory,
    /// The prior selection is no longer in the candidate set — Go would
    /// re-run `rendezvousSelect` over the current enabled keys.
    RemovedFromCandidates,
    /// The candidate set is empty — Go falls back to `Credentials.APIKeys[0]`.
    /// This variant is only produced by `prefer_sticky_channel` when both the
    /// candidate list and the historical choice are unusable; the caller is
    /// responsible for supplying the channel's full credentials list as the
    /// ultimate fallback (mirroring `Credentials.APIKeys[0]`).
    NoCandidates,
}

/// S15 — pure sticky-channel decision. Given an optional historical sticky
/// selection (`history`) and the *current* candidate set (`candidates`),
/// prefer the sticky selection when it is still a candidate; otherwise pick a
/// fresh deterministic choice via `rendezvous_select`, seeded by `seed`
/// (typically the trace id).
///
/// Mirrors the cache-hit vs cache-miss branches of Go
/// `TraceStickyKeyProvider.Get`:
/// * cache hit AND key still enabled → reuse (Go returns the cached key as-is;
///   note the Go LRU does **not** validate membership on hit, but the
///   `rendezvousSelect` it would otherwise run produces a member of `enabled`
///   by construction, so a cached key can only be stale if the enabled set
///   shrank — which is exactly the case this function lets the caller detect).
/// * cache miss or stale → `rendezvous_select(candidates, seed)`.
///
/// `seed` is ignored on the `Use` path. Returns `None` only when
/// `candidates` is empty AND `history` is `None`/empty — the ultimate
/// "no candidates" case that Go resolves with `Credentials.APIKeys[0]`;
/// the caller owns that fallback because it requires the channel's full
/// credential list, not just the enabled subset.
pub fn prefer_sticky_channel(
    history: Option<&str>,
    candidates: &[String],
    seed: &str,
) -> Option<StickyDecision> {
    // Go short-circuit: a single candidate is always the answer, sticky or not.
    if candidates.len() == 1 {
        let only = candidates[0].clone();
        return match history {
            Some(h) if !h.is_empty() && h == only => Some(StickyDecision::Use { selected: only }),
            _ => Some(StickyDecision::Fallback {
                selected: only,
                reason: StickyFallbackReason::NoHistory,
            }),
        };
    }

    // Sticky reuse: history is present and still a candidate.
    if let Some(h) = history.filter(|h| !h.is_empty())
        && candidates.iter().any(|c| c == h)
    {
        return Some(StickyDecision::Use {
            selected: h.to_owned(),
        });
    }

    // Fallback: recompute via rendezvous hashing over the current candidates.
    let selected = trace_sticky_select(candidates, seed)?;

    let reason = match history {
        None => StickyFallbackReason::NoHistory,
        Some(h) if h.is_empty() => StickyFallbackReason::NoHistory,
        // History was supplied but is no longer in the candidate set.
        Some(_) => StickyFallbackReason::RemovedFromCandidates,
    };

    Some(StickyDecision::Fallback { selected, reason })
}

/// S15 — deterministic Highest Random Weight (Rendezvous) hashing selection.
/// Pure port of Go `rendezvousSelect` over the candidate set, keyed by `seed`
/// (the trace id). Returns `None` only for an empty candidate list.
///
/// Stability guarantee (matches the Go contract): adding or removing a single
/// candidate remaps at most ~1/N of seeds; the same `(candidates, seed)` pair
/// always yields the same selection. This is what makes the sticky cache safe —
/// a recompute after a key-set change usually picks the same key again.
///
/// This is a thin borrow-returning wrapper over `crate::rendezvous_select`
/// (defined in `channel_service`), which is the canonical 1:1 port of Go
/// `rendezvousSelect` + `hashAPIKey`. The wrapper exists so the sticky-channel
/// module has a self-named entry point returning an owned `String`, which is
/// what `StickyDecision` carries; the underlying hash + selection algorithm is
/// shared so the two ports cannot drift.
pub fn trace_sticky_select(candidates: &[String], seed: &str) -> Option<String> {
    crate::rendezvous_select(candidates, seed).map(str::to_owned)
}

// =========================================================================
// S16 — trace content/preview project-scoped authorization. Pure.
//
// Mirrors the project-scope guard Go applies on every trace/request content
// and preview endpoint:
//   * `api/request_content.go::DownloadRequestContent` —
//         if projectID != req.ProjectID { JSONError(c, http.StatusNotFound, ...) }
//   * `api/request_live.go::PreviewRequest` —
//         if req.ProjectID != projectID { JSONError(c, http.StatusNotFound, ...) }
//
// The Go HTTP layer deliberately returns **404 NotFound** (not 403) on a
// cross-project mismatch so the server does not leak the existence of another
// project's trace. This pure primitive returns `ConduitError::forbidden` so the
// authorization decision is semantically honest at the service layer; the HTTP
// handler is responsible for translating it to a 404 when crossing the wire
// (mirroring the Go `JSONError(c, http.StatusNotFound, ...)` calls above).
//
// Go quote (`api/request_live.go`, lines 122-125):
//   ```go
//   if req.ProjectID != projectID {
//       JSONError(c, http.StatusNotFound, errors.New("Request not found"))
//       return
//   }
//   ```
// =========================================================================

/// S16 — authorize access to a trace (or its content/preview) by project.
///
/// `principal_project` is the project id derived from the caller's
/// authenticated context (Go `contexts.GetProjectID`); `trace_project` is the
/// project that owns the trace row being read. On mismatch this returns
/// `ConduitError::forbidden`; the HTTP layer is expected to map that to a 404 to
/// avoid leaking cross-project existence, matching Go's `request_content.go`
/// and `request_live.go` guards.
///
/// Project ids are compared as-is; callers are responsible for any
/// normalization. Empty principal project ids are treated as unauthorized
/// (the Go handlers reject `projectID <= 0` up front with a 400, which never
/// reaches this primitive).
pub fn authorize_trace_access(
    principal_project: i64,
    trace_project: i64,
) -> Result<(), conduit_core::ConduitError> {
    if principal_project == trace_project {
        Ok(())
    } else {
        Err(conduit_core::ConduitError::forbidden(
            "trace does not belong to the caller's project",
        ))
    }
}

// =========================================================================
// S17 — pure trace segment / span tree-building logic. Mirrors Go
// `internal/server/biz/trace.go` span-keying, dedup, prefix-match and
// parent-finding functions (lines 1370-1566). These are pure and do NOT
// depend on the ent ORM or the LLM transformer pipeline — they operate on
// already-extracted `Span` values. The DB-backed `GetRootSegment` orchestration
// (Go lines 382-489) and `requestToSegment` body→spans extraction (Go lines
// 574-666, requires inbound/outbound transformers) are NOT ported here; their
// Go tests (`TestTraceService_GetRequestTrace*`, `TestTraceService_GetRootSegment_*`)
// are DB-backed and remain pending the transformer pipeline port.
//
// The pure functions below are exercised by the Go pure-logic tests:
//   * `TestSpanToKey_CompactTypesIncludeSummary` (trace_test.go:673)
//   * `TestDeduplicateSpansWithParent_CompactSummaryUsesContentKey` (trace_test.go:651)
// and the tree-building scenarios behind the DB-backed
// `TestTraceService_GetRootSegment_*` tests are mirrored here as pure
// `SegmentBuildInfo`-only tests (no DB, no transformer).
// =========================================================================

/// Mirrors Go `biz.SpanSystemInstruction` (trace.go:289).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanSystemInstruction {
    pub instruction: String,
}

/// Mirrors Go `biz.SpanUserQuery` (trace.go:293).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanUserQuery {
    pub text: String,
}

/// Mirrors Go `biz.SpanUserImageURL` (trace.go:297).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanUserImageURL {
    pub url: String,
}

/// Mirrors Go `biz.SpanUserVideoURL` (trace.go:301).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanUserVideoURL {
    pub url: String,
}

/// Mirrors Go `biz.SpanUserInputAudio` (trace.go:305).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanUserInputAudio {
    pub format: String,
    pub data: String,
}

/// Mirrors Go `biz.SpanThinking` (trace.go:310).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanThinking {
    pub thinking: String,
}

/// Mirrors Go `biz.SpanText` (trace.go:314).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanText {
    pub text: String,
}

/// Mirrors Go `biz.SpanImageURL` (trace.go:318).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanImageURL {
    pub url: String,
}

/// Mirrors Go `biz.SpanVideoURL` (trace.go:322).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanVideoURL {
    pub url: String,
}

/// Mirrors Go `biz.SpanAudio` (trace.go:326).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanAudio {
    pub id: String,
    pub format: String,
    pub data: String,
    pub transcript: String,
}

/// Mirrors Go `biz.SpanToolUse` (trace.go:333).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanToolUse {
    pub id: String,
    pub r#type: String,
    pub name: String,
    pub arguments: Option<String>,
}

/// Mirrors Go `biz.SpanToolResult` (trace.go:340). `tool_call_id` maps to Go
/// `ToolCallID` (json tag `id`); `is_error` maps to Go `IsError` (json `error`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanToolResult {
    pub tool_call_id: String,
    pub is_error: bool,
    pub text: Option<String>,
}

/// Mirrors Go `biz.SpanCompaction` (trace.go:348).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanCompaction {
    pub summary: String,
}

/// Mirrors Go `biz.SpanValue` (trace.go:273). All sub-values are optional; only
/// one is populated per span in practice (the `Type` discriminates which).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanValue {
    pub system_instruction: Option<SpanSystemInstruction>,
    pub user_query: Option<SpanUserQuery>,
    pub user_image_url: Option<SpanUserImageURL>,
    pub user_video_url: Option<SpanUserVideoURL>,
    pub user_input_audio: Option<SpanUserInputAudio>,
    pub text: Option<SpanText>,
    pub thinking: Option<SpanThinking>,
    pub image_url: Option<SpanImageURL>,
    pub video_url: Option<SpanVideoURL>,
    pub audio: Option<SpanAudio>,
    pub tool_use: Option<SpanToolUse>,
    pub tool_result: Option<SpanToolResult>,
    pub compaction: Option<SpanCompaction>,
}

/// Mirrors Go `biz.Span` (trace.go:252). `start_time`/`end_time` are retained
/// for parity but the pure tree-building logic does not consult them (only
/// `find_segment_parent` reads `start_time` for the tool-call-id tie-break).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub id: String,
    pub r#type: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub value: Option<SpanValue>,
}

/// Mirrors Go `biz.segmentBuildInfo` (trace.go:1371). Holds the intermediate
/// data needed to build the segment tree: the segment's spans, the tool_call_ids
/// its response *produces* (tool_use) and its request *consumes* (tool_result),
/// plus the original combined spans used as the dedup prefix target.
///
/// `segment_id` stands in for the Go `segment.ID` (the request row's primary
/// key) — it is an opaque identifier the caller uses to correlate segments.
#[derive(Debug, Clone)]
pub struct SegmentBuildInfo {
    pub segment_id: String,
    pub segment_start_time: DateTime<Utc>,
    /// Original request + response spans (Go `originSpans`). Used as the
    /// dedup prefix target for children (Go `deduplicateSpansWithParent`).
    pub origin_spans: Vec<Span>,
    /// Original request spans only (Go `originRequestSpans`). Used as the
    /// prefix-matching source for `count_common_span_prefix`.
    pub origin_request_spans: Vec<Span>,
    /// tool_call_ids produced in the response (tool_use spans). Go
    /// `extractProducedToolCallIDs`.
    pub produced_tool_call_ids: std::collections::BTreeSet<String>,
    /// tool_call_ids consumed in the request (tool_result spans). Go
    /// `extractConsumedToolCallIDs`.
    pub consumed_tool_call_ids: std::collections::BTreeSet<String>,
}

impl SegmentBuildInfo {
    /// Build a `SegmentBuildInfo` from a segment's request/response spans,
    /// mirroring the Go constructor inline at trace.go:447-453. `segment_id`
    /// and `segment_start_time` are caller-supplied (Go pulls them from the
    /// `Segment` struct built by `requestToSegment`).
    pub fn from_spans(
        segment_id: impl Into<String>,
        segment_start_time: DateTime<Utc>,
        request_spans: &[Span],
        response_spans: &[Span],
    ) -> Self {
        let mut origin_spans: Vec<Span> =
            Vec::with_capacity(request_spans.len() + response_spans.len());
        origin_spans.extend(request_spans.iter().cloned());
        origin_spans.extend(response_spans.iter().cloned());
        let origin_request_spans: Vec<Span> = request_spans.to_vec();
        let produced_tool_call_ids = extract_produced_tool_call_ids(response_spans);
        let consumed_tool_call_ids = extract_consumed_tool_call_ids(request_spans);
        Self {
            segment_id: segment_id.into(),
            segment_start_time,
            origin_spans,
            origin_request_spans,
            produced_tool_call_ids,
            consumed_tool_call_ids,
        }
    }
}

/// Mirrors Go `biz.extractProducedToolCallIDs` (trace.go:1380). Collects the
/// `id` of every `tool_use` span in `response_spans` (empty ids are skipped,
/// matching Go's `span.Value.ToolUse.ID != ""` guard).
pub fn extract_produced_tool_call_ids(
    response_spans: &[Span],
) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for span in response_spans {
        if span.r#type == "tool_use"
            && let Some(v) = span.value.as_ref()
            && let Some(tu) = v.tool_use.as_ref()
            && !tu.id.is_empty()
        {
            ids.insert(tu.id.clone());
        }
    }
    ids
}

/// Mirrors Go `biz.extractConsumedToolCallIDs` (trace.go:1393). Collects the
/// `tool_call_id` of every `tool_result` span in `request_spans` (empty ids
/// are skipped, matching Go's `span.Value.ToolResult.ToolCallID != ""` guard).
pub fn extract_consumed_tool_call_ids(
    request_spans: &[Span],
) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    for span in request_spans {
        if span.r#type == "tool_result"
            && let Some(v) = span.value.as_ref()
            && let Some(tr) = v.tool_result.as_ref()
            && !tr.tool_call_id.is_empty()
        {
            ids.insert(tr.tool_call_id.clone());
        }
    }
    ids
}

/// Mirrors Go `biz.spanToKey` (trace.go:1499). Generates a unique key for a
/// span based on its content. Spans with no `Value` (or an unmatched type)
/// collapse to `"<type>:"`. The compact types (`compaction`, `compaction_summary`)
/// include the `Compaction.Summary` in the key — this is what makes
/// `deduplicate_spans_with_parent` keep the *new* compaction summary when a
/// parent and child carry different summaries (Go
/// `TestDeduplicateSpansWithParent_CompactSummaryUsesContentKey`).
///
/// Format matches Go byte-for-byte for each branch (order matters: `tool_use`
/// is `<type>:<id>:<name>:<args>`, `tool_result` is
/// `<type>:<tool_call_id>:<is_error>:<text>`).
pub fn span_to_key(span: &Span) -> String {
    let Some(value) = span.value.as_ref() else {
        return format!("{}:", span.r#type);
    };

    match span.r#type.as_str() {
        "user_query" => {
            if let Some(uq) = value.user_query.as_ref() {
                return format!("{}:{}", span.r#type, uq.text);
            }
        }
        "user_image_url" => {
            if let Some(u) = value.user_image_url.as_ref() {
                return format!("{}:{}", span.r#type, u.url);
            }
        }
        "user_video_url" => {
            if let Some(u) = value.user_video_url.as_ref() {
                return format!("{}:{}", span.r#type, u.url);
            }
        }
        "user_input_audio" => {
            if let Some(u) = value.user_input_audio.as_ref() {
                return format!("{}:{}:{}", span.r#type, u.format, u.data);
            }
        }
        "text" => {
            if let Some(t) = value.text.as_ref() {
                return format!("{}:{}", span.r#type, t.text);
            }
        }
        "thinking" => {
            if let Some(t) = value.thinking.as_ref() {
                return format!("{}:{}", span.r#type, t.thinking);
            }
        }
        "image_url" => {
            if let Some(u) = value.image_url.as_ref() {
                return format!("{}:{}", span.r#type, u.url);
            }
        }
        "video_url" => {
            if let Some(u) = value.video_url.as_ref() {
                return format!("{}:{}", span.r#type, u.url);
            }
        }
        "audio" => {
            if let Some(a) = value.audio.as_ref() {
                return format!("{}:{}:{}:{}", span.r#type, a.id, a.format, a.transcript);
            }
        }
        "compaction" | "compaction_summary" => {
            if let Some(c) = value.compaction.as_ref() {
                return format!("{}:{}", span.r#type, c.summary);
            }
        }
        "tool_use" => {
            if let Some(tu) = value.tool_use.as_ref() {
                let args = tu.arguments.as_deref().unwrap_or("");
                return format!("{}:{}:{}:{}", span.r#type, tu.id, tu.name, args);
            }
        }
        "tool_result" => {
            if let Some(tr) = value.tool_result.as_ref() {
                let output = tr.text.as_deref().unwrap_or("");
                return format!(
                    "{}:{}:{}:{}",
                    span.r#type, tr.tool_call_id, tr.is_error, output
                );
            }
        }
        _ => {}
    }

    format!("{}:", span.r#type)
}

/// Mirrors Go `biz.deduplicateSpansWithParent` (trace.go:1466). Removes spans
/// from `current` whose key matches the span at the same index in `parent`.
/// Subsequent requests in a trace carry previous context messages as a prefix;
/// this strips that shared prefix so only the *new* spans remain.
///
/// Index-aligned: when `current[i]`'s key equals `parent[i]`'s key, it is
/// dropped; once a mismatch is found, the rest of `current` is kept verbatim
/// (matching Go's `i >= len(parent)` short-circuit + the per-index equality
/// check). Empty `current` or `parent` returns `current` unchanged.
pub fn deduplicate_spans_with_parent(current: &[Span], parent: &[Span]) -> Vec<Span> {
    if current.is_empty() || parent.is_empty() {
        return current.to_vec();
    }

    let mut result: Vec<Span> = Vec::with_capacity(current.len().saturating_sub(parent.len()));
    for (i, span) in current.iter().enumerate() {
        if i >= parent.len() {
            result.push(span.clone());
            continue;
        }
        if span_to_key(span) == span_to_key(&parent[i]) {
            continue;
        }
        result.push(span.clone());
    }
    result
}

/// Mirrors Go `biz.countCommonSpanPrefix` (trace.go:1449). Counts the number
/// of matching spans from the start of both slices (key equality via
/// `span_to_key`). Stops at the first mismatch.
pub fn count_common_span_prefix(current: &[Span], predecessor: &[Span]) -> usize {
    let max_len = current.len().min(predecessor.len());
    let mut count = 0;
    for i in 0..max_len {
        if span_to_key(&current[i]) != span_to_key(&predecessor[i]) {
            break;
        }
        count += 1;
    }
    count
}

/// Mirrors Go `biz.findSegmentParent` (trace.go:1409). Determines the parent
/// for `current` using a 3-tier strategy:
///  1. **Tool call ID matching**: find the latest (by `segment_start_time`)
///     predecessor whose `produced_tool_call_ids` intersects `current`'s
///     `consumed_tool_call_ids`. "Latest" = greatest `segment_start_time`,
///     matching Go's `producer.segment.StartTime.After(latestProducer.segment.StartTime)`.
///  2. **Span prefix matching**: find the predecessor with the longest common
///     request-span prefix (via `count_common_span_prefix` against
///     `current.origin_request_spans` vs `pred.origin_spans`). Ties resolve to
///     the *first* predecessor with the best length (Go iterates in order and
///     only replaces on a strictly greater `matchLen`).
///  3. **Fallback**: the chronologically nearest previous segment = the last
///     entry in `predecessors` (Go: `predecessors[len(predecessors)-1]`).
///
/// `tool_call_index` maps a produced tool_call_id → the predecessor that
/// produced it (Go maintains this incrementally as segments are processed).
/// Returns the chosen predecessor by reference.
///
/// Returns `None` only when `predecessors` is empty (the caller is expected to
/// have skipped the root segment; Go never calls this for `i == 0`).
pub fn find_segment_parent<'a>(
    current: &SegmentBuildInfo,
    predecessors: &'a [SegmentBuildInfo],
    tool_call_index: &std::collections::BTreeMap<String, usize>,
) -> Option<&'a SegmentBuildInfo> {
    if predecessors.is_empty() {
        return None;
    }

    // Strategy 1: tool_call_id matching — latest producer by start_time.
    if !current.consumed_tool_call_ids.is_empty() {
        let mut latest: Option<(usize, DateTime<Utc>)> = None;
        for id in &current.consumed_tool_call_ids {
            if let Some(&idx) = tool_call_index.get(id) {
                let candidate_time = predecessors[idx].segment_start_time;
                match latest {
                    None => latest = Some((idx, candidate_time)),
                    Some((_, best_time)) if candidate_time > best_time => {
                        latest = Some((idx, candidate_time));
                    }
                    _ => {}
                }
            }
        }
        if let Some((idx, _)) = latest {
            return Some(&predecessors[idx]);
        }
    }

    // Strategy 2: span prefix matching — longest common prefix wins.
    let mut best_idx: Option<usize> = None;
    let mut best_match_len: usize = 0;
    for (i, pred) in predecessors.iter().enumerate() {
        let match_len = count_common_span_prefix(&current.origin_request_spans, &pred.origin_spans);
        if match_len > best_match_len {
            best_match_len = match_len;
            best_idx = Some(i);
        }
    }
    if let Some(i) = best_idx {
        return Some(&predecessors[i]);
    }

    // Strategy 3: fallback to the chronologically nearest previous segment.
    Some(&predecessors[predecessors.len() - 1])
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use crate::{InMemoryThreadServiceRepo, ThreadService};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn same_project_external_id_is_idempotent() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo.clone());
        let ctx = ctx();

        let first = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;
        let second = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;

        assert_eq!(first, second);
        assert_eq!(repo.trace_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn different_projects_are_isolated() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo.clone());
        let ctx = ctx();

        let project_a = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;
        let project_b = service
            .get_or_create_trace(&ctx, "project-b", "trace-ext-1", None)
            .await?;

        assert_ne!(project_a.id, project_b.id);
        assert_eq!(project_a.external_id, project_b.external_id);
        assert_eq!(repo.trace_count()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn trace_can_reference_thread() -> TraceServiceResult<()> {
        let thread_service = ThreadService::new(Arc::new(InMemoryThreadServiceRepo::new()));
        let trace_service = TraceService::new(Arc::new(InMemoryTraceServiceRepo::new()));
        let ctx = ctx();

        let thread = thread_service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await
            .map_err(|_| TraceServiceError::LockPoisoned)?;
        let trace = trace_service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", Some(thread.id.clone()))
            .await?;

        assert_eq!(trace.thread_id.as_deref(), Some(thread.id.as_str()));
        Ok(())
    }

    #[tokio::test]
    async fn thread_id_is_immutable_after_first_create() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo.clone());
        let ctx = ctx();

        // First create wins the thread linkage.
        let first = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", Some("thread-1".into()))
            .await?;
        // A later call supplies a different thread_id; the original value is
        // preserved (mirrors Go's Immutable thread_id field).
        let second = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", Some("thread-2".into()))
            .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(second.thread_id.as_deref(), Some("thread-1"));
        assert_eq!(repo.trace_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn get_trace_returns_some_for_existing() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo);
        let ctx = ctx();

        let created = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;
        let found = service.get_trace(&ctx, "project-a", "trace-ext-1").await?;

        assert_eq!(found, Some(created));
        Ok(())
    }

    #[tokio::test]
    async fn get_trace_returns_none_for_missing() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo);
        let ctx = ctx();

        let found = service
            .get_trace(&ctx, "project-a", "never-created")
            .await?;
        assert!(found.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_trace_does_not_leak_across_projects() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo);
        let ctx = ctx();

        service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;
        // The same external_id in project-b is a distinct row; project-b's
        // lookup must not see project-a's record.
        let cross = service.get_trace(&ctx, "project-b", "trace-ext-1").await?;
        assert!(cross.is_none());
        Ok(())
    }

    // =====================================================================
    // S13 — thread_id external-string → internal Thread row mapping, and
    // trace_id ↔ thread association. Mirrors Go biz/thread.go +
    // biz/trace.go::GetOrCreateTrace (which takes `threadID *int` — the
    // internal Thread.ID primary key, NOT the external string; the
    // WithThread middleware resolves the string→row mapping first via
    // ThreadService.GetOrCreateThread, then WithTrace reads the resolved
    // thread from context and passes `&thread.ID`).
    // =====================================================================

    // Mirrors Go `TestTraceService_GetOrCreateTrace` (no-thread branch): a
    // trace created with `threadID = nil` has no thread linkage — `thread_id`
    // is `None` on the row, matching Go's zero-value `ThreadID` (0, which the
    // Rust port represents as `Option::None`).
    #[tokio::test]
    async fn s13_trace_without_thread_has_no_thread_linkage() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo);
        let ctx = ctx();

        let trace = service
            .get_or_create_trace(&ctx, "project-a", "trace-ext-1", None)
            .await?;
        assert!(trace.thread_id.is_none());
        Ok(())
    }

    // S13 — the trace→thread association is keyed on the *internal* Thread.ID
    // (Go `&thread.ID`), and the same internal id links all traces in that
    // thread within the project. Two traces referencing the same internal
    // thread id store the same `thread_id` value.
    #[tokio::test]
    async fn s13_two_traces_can_reference_same_internal_thread_id() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo);
        let ctx = ctx();

        let thread_internal_id = "thread:project-a:thread-ext-1".to_string();
        let t1 = service
            .get_or_create_trace(
                &ctx,
                "project-a",
                "trace-1",
                Some(thread_internal_id.clone()),
            )
            .await?;
        let t2 = service
            .get_or_create_trace(
                &ctx,
                "project-a",
                "trace-2",
                Some(thread_internal_id.clone()),
            )
            .await?;

        assert_ne!(t1.id, t2.id, "distinct trace_id → distinct trace rows");
        assert_eq!(t1.thread_id.as_deref(), Some(thread_internal_id.as_str()));
        assert_eq!(t2.thread_id.as_deref(), Some(thread_internal_id.as_str()));
        Ok(())
    }

    // S13 — project isolation of the trace↔thread association: the same
    // internal thread id string in two different projects is an independent
    // association (Go keys on `(trace_id, project_id)`; the `threadID *int`
    // is whatever internal Thread.ID the WithThread middleware resolved in
    // *that* project). The trace row in project-b does not see project-a's
    // thread linkage.
    #[tokio::test]
    async fn s13_trace_thread_association_is_project_scoped() -> TraceServiceResult<()> {
        let repo = Arc::new(InMemoryTraceServiceRepo::new());
        let service = TraceService::new(repo.clone());
        let ctx = ctx();

        // Both projects use the same external thread string "thread-ext-1",
        // which WithThread would resolve to distinct internal Thread.IDs in
        // each project. Here we simulate the post-resolution state by passing
        // the distinct internal ids directly.
        service
            .get_or_create_trace(
                &ctx,
                "project-a",
                "trace-shared",
                Some("thread:project-a:thread-ext-1".into()),
            )
            .await?;
        service
            .get_or_create_trace(
                &ctx,
                "project-b",
                "trace-shared",
                Some("thread:project-b:thread-ext-1".into()),
            )
            .await?;

        let a = service.get_trace(&ctx, "project-a", "trace-shared").await?;
        let b = service.get_trace(&ctx, "project-b", "trace-shared").await?;
        let a = a.ok_or(TraceServiceError::LockPoisoned)?;
        let b = b.ok_or(TraceServiceError::LockPoisoned)?;

        assert_eq!(
            a.thread_id.as_deref(),
            Some("thread:project-a:thread-ext-1")
        );
        assert_eq!(
            b.thread_id.as_deref(),
            Some("thread:project-b:thread-ext-1")
        );
        assert_eq!(repo.trace_count()?, 2);
        Ok(())
    }

    // S13 — end-to-end pure simulation of the WithThread → WithTrace wiring
    // (mirrors Go middleware/thread.go + middleware/trace.go::WithTrace lines
    // 121-125): the external thread_id *string* from the header is mapped to
    // an internal Thread row by ThreadService, then the internal row id is
    // passed as the `thread_id` to TraceService.get_or_create_trace. The
    // stored trace.thread_id equals the *internal* id, not the external
    // string.
    #[tokio::test]
    async fn s13_external_thread_string_maps_to_internal_row_for_trace_linkage()
    -> Result<(), Box<dyn std::error::Error>> {
        let thread_service = ThreadService::new(Arc::new(InMemoryThreadServiceRepo::new()));
        let trace_service = TraceService::new(Arc::new(InMemoryTraceServiceRepo::new()));
        let ctx = ctx();

        // 1. WithThread resolves the external string → internal Thread row.
        let external_thread_id = "thread-from-header-xyz";
        let thread_row = thread_service
            .get_or_create_thread(&ctx, "project-a", external_thread_id)
            .await
            .map_err(|_| "thread lock poisoned")?;
        // The internal id is opaque to callers — it is NOT the external string.
        assert_ne!(thread_row.id, external_thread_id);
        assert!(thread_row.id.starts_with("thread:project-a:"));

        // 2. WithTrace passes the *internal* thread row id (Go: `&thread.ID`)
        //    — not the external string — to GetOrCreateTrace.
        let trace = trace_service
            .get_or_create_trace(
                &ctx,
                "project-a",
                "trace-ext-1",
                Some(thread_row.id.clone()),
            )
            .await
            .map_err(|_| "trace lock poisoned")?;

        // 3. The association stores the internal id, mirroring Go's
        //    `trace.ThreadID = thread.ID` (an int primary key in Go, a
        //    scoped string id here).
        assert_eq!(trace.thread_id.as_deref(), Some(thread_row.id.as_str()));
        assert_ne!(
            trace.thread_id.as_deref(),
            Some(external_thread_id),
            "stored thread_id must be the internal row id, not the external string"
        );
        Ok(())
    }

    // =====================================================================
    // Header / body extraction — mirrors Go middleware/trace_test.go intent
    // =====================================================================

    fn hdr(name: &str, val: &str) -> (String, String) {
        (name.to_owned(), val.to_owned())
    }

    // S05 — default header name and effective header override.
    #[test]
    fn effective_trace_header_defaults_to_conduit_trace_id() {
        let cfg = TracingConfig::default();
        assert_eq!(cfg.effective_trace_header(), "Conduit-Trace-Id");
    }

    #[test]
    fn effective_trace_header_uses_override_when_set() {
        let cfg = TracingConfig {
            trace_header: "X-Trace-Id".into(),
            ..Default::default()
        };
        assert_eq!(cfg.effective_trace_header(), "X-Trace-Id");
    }

    // Mirrors Go `TestWithTraceID_Success` header read.
    #[test]
    fn extract_trace_id_from_headers_reads_default_header() {
        let headers = [hdr("Conduit-Trace-Id", "trace-test-123")];
        let cfg = TracingConfig::default();
        assert_eq!(
            extract_trace_id_from_headers(&headers, &cfg),
            Some("trace-test-123".into())
        );
    }

    // Mirrors Go `TestWithTraceID_Success` — header lookup is case-insensitive
    // (Go http.Header.Get canonicalizes).
    #[test]
    fn extract_trace_id_from_headers_is_case_insensitive() {
        let headers = [hdr("Conduit-Trace-Id", "trace-test-123")];
        let cfg = TracingConfig::default();
        assert_eq!(
            extract_trace_id_from_headers(&headers, &cfg),
            Some("trace-test-123".into())
        );
    }

    #[test]
    fn extract_trace_id_from_headers_returns_none_when_absent() {
        let headers = [];
        let cfg = TracingConfig::default();
        assert_eq!(extract_trace_id_from_headers(&headers, &cfg), None);
    }

    #[test]
    fn extract_trace_id_from_headers_trims_whitespace() {
        let headers = [hdr("Conduit-Trace-Id", "  trace-1  ")];
        let cfg = TracingConfig::default();
        assert_eq!(
            extract_trace_id_from_headers(&headers, &cfg),
            Some("trace-1".into())
        );
    }

    #[test]
    fn extract_trace_id_from_headers_treats_empty_as_absent() {
        let headers = [hdr("Conduit-Trace-Id", "")];
        let cfg = TracingConfig::default();
        assert_eq!(extract_trace_id_from_headers(&headers, &cfg), None);
    }

    // S06 — extra trace headers fallback, in declaration order.
    #[test]
    fn extract_trace_id_from_headers_falls_back_to_extra_headers_in_order() {
        let headers = [
            hdr("Sentry-Trace", "sentry-1"),
            hdr("X-Extra-Trace", "extra-1"),
        ];
        let cfg = TracingConfig {
            extra_trace_headers: vec!["Missing-Trace".into(), "Sentry-Trace".into()],
            ..Default::default()
        };
        // First non-empty extra in order wins — "Missing-Trace" is absent so
        // we fall through to "Sentry-Trace".
        assert_eq!(
            extract_trace_id_from_headers(&headers, &cfg),
            Some("sentry-1".into())
        );
    }

    #[test]
    fn extract_trace_id_from_headers_primary_beats_extra() {
        let headers = [
            hdr("Conduit-Trace-Id", "primary"),
            hdr("Sentry-Trace", "extra"),
        ];
        let cfg = TracingConfig {
            extra_trace_headers: vec!["Sentry-Trace".into()],
            ..Default::default()
        };
        assert_eq!(
            extract_trace_id_from_headers(&headers, &cfg),
            Some("primary".into())
        );
    }

    // S09 — OpenCode `x-session-affinity` extraction (case-insensitive header).
    #[test]
    fn extract_trace_id_from_opencode_reads_session_affinity() {
        let headers = [hdr("X-Session-Affinity", "opencode-session-123")];
        assert_eq!(
            extract_trace_id_from_opencode(&headers),
            Some("opencode-session-123".into())
        );
    }

    #[test]
    fn extract_trace_id_from_opencode_returns_none_when_absent() {
        let headers: [(String, String); 0] = [];
        assert_eq!(extract_trace_id_from_opencode(&headers), None);
    }

    #[test]
    fn extract_trace_id_from_opencode_returns_none_when_empty() {
        let headers = [hdr("x-session-affinity", "   ")];
        assert_eq!(extract_trace_id_from_opencode(&headers), None);
    }

    // S08 — Codex `Session_id` header wins over turn metadata.
    #[test]
    fn extract_trace_id_from_codex_prefers_session_header() {
        let headers = [
            hdr("Session_id", "codex-session-123"),
            hdr(
                "X-Codex-Turn-Metadata",
                r#"{"session_id":"codex-turn-session-123","turn_id":"turn-1"}"#,
            ),
        ];
        assert_eq!(
            extract_trace_id_from_codex(&headers),
            Some("codex-session-123".into())
        );
    }

    #[test]
    fn extract_trace_id_from_codex_falls_back_to_turn_metadata() {
        let headers = [hdr(
            "X-Codex-Turn-Metadata",
            r#"{"session_id":"codex-turn-session-123","turn_id":"turn-1"}"#,
        )];
        assert_eq!(
            extract_trace_id_from_codex(&headers),
            Some("codex-turn-session-123".into())
        );
    }

    #[test]
    fn extract_trace_id_from_codex_returns_none_when_both_absent() {
        let headers: [(String, String); 0] = [];
        assert_eq!(extract_trace_id_from_codex(&headers), None);
    }

    // Mirrors Go `TestWithTrace_CodexTurnMetadataInvalidOrMissingSessionDoesNotSetTrace`.
    #[test]
    fn extract_codex_turn_metadata_returns_none_for_invalid_payloads() {
        for raw in [
            "",
            "   ",
            "{\"session_id\":",
            r#"{"turn_id":"turn-1"}"#,
            r#"{"session_id":"   ","turn_id":"turn-1"}"#,
        ] {
            assert_eq!(
                extract_codex_turn_metadata_session_id(raw),
                None,
                "raw = {raw:?}"
            );
        }
    }

    // S07 — Claude Code `metadata.user_id` extraction.
    // Mirrors Go `TestExtractClaudeTraceID` golden cases.
    #[test]
    fn parse_claude_code_user_id_legacy_format() {
        let raw = "user_20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad_account__session_f25958b8-e75c-455d-8b40-f006d87cc2a4";
        let uid = parse_claude_code_user_id(raw);
        assert_eq!(
            uid,
            Some(ClaudeCodeUserId {
                device_id: "20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad"
                    .into(),
                account_uuid: "".into(),
                session_id: "f25958b8-e75c-455d-8b40-f006d87cc2a4".into(),
            })
        );
    }

    #[test]
    fn parse_claude_code_user_id_v2_json_format() {
        let raw = r#"{"device_id":"67bad5aabbccdd1122334455667788990011223344556677889900aabbccddee","account_uuid":"","session_id":"f25958b8-e75c-455d-8b40-f006d87cc2a4"}"#;
        let uid = parse_claude_code_user_id(raw);
        assert_eq!(
            uid.map(|u| u.session_id),
            Some("f25958b8-e75c-455d-8b40-f006d87cc2a4".into())
        );
    }

    #[test]
    fn parse_claude_code_user_id_rejects_invalid_inputs() {
        for raw in ["", "   ", "user_123_account__session_456", "not-a-user-id"] {
            assert_eq!(parse_claude_code_user_id(raw), None, "raw = {raw:?}");
        }
    }

    #[test]
    fn parse_claude_code_user_id_v2_requires_non_empty_session_id() {
        let raw = r#"{"device_id":"abc","session_id":""}"#;
        assert_eq!(parse_claude_code_user_id(raw), None);
    }

    // Claude Code extraction requires POST + Messages path + valid user_id.
    #[test]
    fn extract_trace_id_from_claude_code_happy_path() {
        let body: serde_json::Value = serde_json::json!({
            "metadata": {
                "user_id": "user_20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad_account__session_f25958b8-e75c-455d-8b40-f006d87cc2a4"
            }
        });
        assert_eq!(
            extract_trace_id_from_claude_code("POST", "/v1/messages", Some(&body)),
            Some("f25958b8-e75c-455d-8b40-f006d87cc2a4".into())
        );
    }

    #[test]
    fn extract_trace_id_from_claude_code_accepts_anthropic_path() {
        let body: serde_json::Value = serde_json::json!({"metadata":{"user_id":
            "user_20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad_account__session_f25958b8-e75c-455d-8b40-f006d87cc2a4"
        }});
        assert_eq!(
            extract_trace_id_from_claude_code("POST", "/anthropic/v1/messages", Some(&body)),
            Some("f25958b8-e75c-455d-8b40-f006d87cc2a4".into())
        );
    }

    #[test]
    fn extract_trace_id_from_claude_code_rejects_non_post() {
        let body: serde_json::Value = serde_json::json!({"metadata":{"user_id":"x"}});
        assert_eq!(
            extract_trace_id_from_claude_code("GET", "/v1/messages", Some(&body)),
            None
        );
    }

    #[test]
    fn extract_trace_id_from_claude_code_rejects_non_messages_path() {
        let body: serde_json::Value = serde_json::json!({"metadata":{"user_id":"x"}});
        assert_eq!(
            extract_trace_id_from_claude_code("POST", "/v1/chat/completions", Some(&body)),
            None
        );
    }

    #[test]
    fn extract_trace_id_from_claude_code_rejects_invalid_user_id() {
        let body: serde_json::Value = serde_json::json!({"metadata":{"user_id":"user_123"}});
        assert_eq!(
            extract_trace_id_from_claude_code("POST", "/v1/messages", Some(&body)),
            None
        );
    }

    // S06 — extra body fields dotted-path extraction.
    #[test]
    fn extract_trace_id_from_body_reads_top_level_field() {
        let body = serde_json::json!({"trace_id":"trace-from-body-123","message":"x"});
        let fields = vec!["trace_id".into(), "metadata.trace_id".into()];
        assert_eq!(
            extract_trace_id_from_body(&body, &fields),
            Some("trace-from-body-123".into())
        );
    }

    #[test]
    fn extract_trace_id_from_body_reads_nested_field() {
        let body = serde_json::json!({"metadata":{"trace_id":"nested-trace-456"}});
        let fields = vec!["metadata.trace_id".into()];
        assert_eq!(
            extract_trace_id_from_body(&body, &fields),
            Some("nested-trace-456".into())
        );
    }

    #[test]
    fn extract_trace_id_from_body_walks_fields_in_order() {
        let body = serde_json::json!({"trace_id":"first","metadata":{"trace_id":"second"}});
        // First present field wins.
        let fields = vec!["trace_id".into(), "metadata.trace_id".into()];
        assert_eq!(
            extract_trace_id_from_body(&body, &fields),
            Some("first".into())
        );
    }

    #[test]
    fn extract_trace_id_from_body_skips_empty_values() {
        let body = serde_json::json!({"trace_id":"","metadata":{"trace_id":"real"}});
        let fields = vec!["trace_id".into(), "metadata.trace_id".into()];
        assert_eq!(
            extract_trace_id_from_body(&body, &fields),
            Some("real".into())
        );
    }

    #[test]
    fn extract_trace_id_from_body_returns_none_when_missing() {
        let body = serde_json::json!({"foo":"bar"});
        let fields = vec!["trace_id".into()];
        assert_eq!(extract_trace_id_from_body(&body, &fields), None);
    }

    #[test]
    fn extract_trace_id_from_body_returns_none_for_non_string_value() {
        // gjson would return the number as a string, but Go's configured paths
        // are documented for string trace ids; we deliberately require str to
        // match the Go `result.String()` semantics on a string-typed field.
        let body = serde_json::json!({"trace_id":123});
        let fields = vec!["trace_id".into()];
        assert_eq!(extract_trace_id_from_body(&body, &fields), None);
    }

    // S06 — full priority chain. Mirrors the WithTrace ordering.
    #[test]
    fn resolve_trace_id_prefers_primary_header() {
        let headers = [
            hdr("Conduit-Trace-Id", "primary-trace-789"),
            hdr("X-Session-Affinity", "opencode-456"),
        ];
        let cfg = TracingConfig {
            opencode_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/messages", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("primary-trace-789"));
        assert_eq!(r.source, Some(TraceSource::PrimaryHeader));
        assert!(r.enabled);
    }

    // Mirrors Go `TestWithTrace_OpenCodeHeaderHasLowerPriorityThanPrimaryTraceHeader`.
    #[test]
    fn resolve_trace_id_opencode_loses_to_primary_header() {
        let headers = [
            hdr("Conduit-Trace-Id", "primary-trace-789"),
            hdr("X-Session-Affinity", "opencode-session-456"),
        ];
        let cfg = TracingConfig {
            opencode_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("primary-trace-789"));
    }

    // Mirrors Go `TestWithTrace_OpenCodeDisabled` — header present but flag off.
    #[test]
    fn resolve_trace_id_opencode_disabled_returns_id_but_not_enabled() {
        let headers = [hdr("X-Session-Affinity", "opencode-session-123")];
        let cfg = TracingConfig {
            opencode_trace_enabled: false,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("opencode-session-123"));
        assert_eq!(r.source, Some(TraceSource::OpenCode));
        assert!(!r.enabled); // S14: middleware must NOT create a record.
        assert!(!should_record_trace(&r));
    }

    // Mirrors Go `TestWithTrace_OpenCodeHeaderSetsTrace`.
    #[test]
    fn resolve_trace_id_opencode_enabled_when_flag_on() {
        let headers = [hdr("X-Session-Affinity", "opencode-session-123")];
        let cfg = TracingConfig {
            opencode_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("opencode-session-123"));
        assert!(r.enabled);
        assert!(should_record_trace(&r));
    }

    // Mirrors Go `TestWithTrace_ClaudeCodeDisabled` — body has user_id but
    // flag off; the id is still surfaced for logging but not enabled.
    #[test]
    fn resolve_trace_id_claude_code_disabled_returns_id_but_not_enabled() {
        let body = serde_json::json!({"metadata":{"user_id":
            "user_20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad_account__session_f25958b8-e75c-455d-8b40-f006d87cc2a4"
        }});
        let cfg = TracingConfig {
            claude_code_trace_enabled: false,
            ..Default::default()
        };
        let r = resolve_trace_id(&[], Some(&body), "POST", "/anthropic/v1/messages", &cfg);
        assert!(
            r.trace_id.is_some(),
            "id should still be surfaced for logs (S14)"
        );
        assert_eq!(r.source, Some(TraceSource::ClaudeCode));
        assert!(!r.enabled);
        assert!(!should_record_trace(&r));
    }

    // Mirrors Go `TestWithTrace_ClaudeCodeSetsTraceHeader`.
    #[test]
    fn resolve_trace_id_claude_code_enabled_extracts_session() {
        let body = serde_json::json!({"metadata":{"user_id":
            "user_20836b5653ed68aa981604f502c0a491397f6053826a93c953423632578d38ad_account__session_f25958b8-e75c-455d-8b40-f006d87cc2a4"
        }});
        let cfg = TracingConfig {
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&[], Some(&body), "POST", "/v1/messages", &cfg);
        assert_eq!(
            r.trace_id.as_deref(),
            Some("f25958b8-e75c-455d-8b40-f006d87cc2a4")
        );
        assert!(r.enabled);
    }

    // Mirrors Go `TestWithTrace_ClaudeCodePreservesExistingTraceHeader` —
    // explicit header beats Claude Code body extraction.
    #[test]
    fn resolve_trace_id_primary_header_beats_claude_code_body() {
        let body = serde_json::json!({"metadata":{"user_id":"user_123"}});
        let headers = [hdr("Conduit-Trace-Id", "existing-trace")];
        let cfg = TracingConfig {
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, Some(&body), "POST", "/v1/messages", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("existing-trace"));
        assert_eq!(r.source, Some(TraceSource::PrimaryHeader));
    }

    // Mirrors Go `TestWithTrace_CodexDisabled`.
    #[test]
    fn resolve_trace_id_codex_disabled_returns_id_but_not_enabled() {
        let headers = [hdr("Session_id", "codex-session-123")];
        let cfg = TracingConfig {
            codex_trace_enabled: false,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("codex-session-123"));
        assert_eq!(r.source, Some(TraceSource::Codex));
        assert!(!r.enabled);
    }

    // Mirrors Go `TestWithTrace_CodexHeaderSetsTrace`.
    #[test]
    fn resolve_trace_id_codex_enabled_uses_session_header() {
        let headers = [hdr("Session_id", "codex-session-123")];
        let cfg = TracingConfig {
            codex_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("codex-session-123"));
        assert!(r.enabled);
    }

    // Mirrors Go `TestWithTrace_CodexTurnMetadataSetsTrace`.
    #[test]
    fn resolve_trace_id_codex_enabled_uses_turn_metadata() {
        let headers = [hdr(
            "X-Codex-Turn-Metadata",
            r#"{"session_id":"codex-turn-session-123","turn_id":"turn-1"}"#,
        )];
        let cfg = TracingConfig {
            codex_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("codex-turn-session-123"));
        assert!(r.enabled);
    }

    // Mirrors Go `TestWithTrace_CodexSessionMissingDoesNotSetTrace`.
    #[test]
    fn resolve_trace_id_returns_none_when_no_source_matches() {
        let cfg = TracingConfig {
            codex_trace_enabled: true,
            opencode_trace_enabled: true,
            claude_code_trace_enabled: true,
            ..Default::default()
        };
        let r = resolve_trace_id(&[], None, "POST", "/v1/chat/completions", &cfg);
        assert_eq!(r.trace_id, None);
        assert!(!r.enabled);
        assert!(!should_record_trace(&r));
    }

    // Mirrors Go `TestWithTrace_ExtraTraceBodyFields_Priority` — header beats body.
    #[test]
    fn resolve_trace_id_header_beats_extra_body_field() {
        let body = serde_json::json!({"trace_id":"body-trace-789"});
        let headers = [hdr("Conduit-Trace-Id", "header-trace-789")];
        let cfg = TracingConfig {
            extra_trace_body_fields: vec!["trace_id".into()],
            ..Default::default()
        };
        let r = resolve_trace_id(&headers, Some(&body), "POST", "/test", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("header-trace-789"));
        assert_eq!(r.source, Some(TraceSource::PrimaryHeader));
    }

    // Mirrors Go `TestWithTrace_ExtraTraceBodyFields`.
    #[test]
    fn resolve_trace_id_uses_extra_body_field_when_no_header() {
        let body = serde_json::json!({"trace_id":"trace-from-body-123","message":"test"});
        let cfg = TracingConfig {
            extra_trace_body_fields: vec!["trace_id".into(), "metadata.trace_id".into()],
            ..Default::default()
        };
        let r = resolve_trace_id(&[], Some(&body), "POST", "/test", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("trace-from-body-123"));
        assert_eq!(r.source, Some(TraceSource::ExtraBody));
        assert!(r.enabled);
    }

    // Mirrors Go `TestWithTrace_ExtraTraceBodyFields_Nested`.
    #[test]
    fn resolve_trace_id_uses_nested_extra_body_field() {
        let body = serde_json::json!({"metadata":{"trace_id":"nested-trace-456"}});
        let cfg = TracingConfig {
            extra_trace_body_fields: vec!["metadata.trace_id".into()],
            ..Default::default()
        };
        let r = resolve_trace_id(&[], Some(&body), "POST", "/test", &cfg);
        assert_eq!(r.trace_id.as_deref(), Some("nested-trace-456"));
    }

    // S10/S11 — get-or-create decision shape.
    #[test]
    fn decide_trace_get_or_create_creates_when_no_existing() {
        let decision = decide_trace_get_or_create(None, "p1", "trace-1", Some("t1".into()));
        match decision {
            GetOrCreateDecision::Create { record } => {
                assert_eq!(record.project_id, "p1");
                assert_eq!(record.external_id, "trace-1");
                assert_eq!(record.thread_id.as_deref(), Some("t1"));
            }
            GetOrCreateDecision::Reuse { .. } => panic!("expected Create"),
        }
    }

    #[test]
    fn decide_trace_get_or_create_reuses_when_existing() {
        let existing = TraceRecord::new("p1", "trace-1", Some("orig-thread".into()));
        // Caller passes a different thread_id on the retry; the existing row's
        // value wins (Go `Immutable` thread_id).
        let decision = decide_trace_get_or_create(
            Some(existing.clone()),
            "p1",
            "trace-1",
            Some("new-thread".into()),
        );
        match decision {
            GetOrCreateDecision::Reuse { existing: row } => {
                assert_eq!(row.thread_id.as_deref(), Some("orig-thread"));
                assert_eq!(row.id, existing.id);
            }
            GetOrCreateDecision::Create { .. } => panic!("expected Reuse"),
        }
    }

    // =====================================================================
    // S15 — sticky-channel / sticky-key selection.
    // Mirrors Go `TestTraceStickyKeyProvider_*` golden intent
    // (`channel_apikey_test.go`): sticky reuse when still enabled, fallback
    // when removed, deterministic per-seed selection, and the degenerate
    // single/empty candidate cases.
    // =====================================================================

    fn candidates(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|s| (*s).to_owned()).collect()
    }

    // Mirrors Go `TestTraceStickyKeyProvider_MultipleKeys_WithTrace_Sticky`:
    // the same trace id always resolves to the same key (via the rendezvous
    // fallback), since the cache-miss recompute is deterministic.
    #[test]
    fn prefer_sticky_channel_no_history_picks_deterministically() {
        let cands = candidates(&["key-1", "key-2", "key-3"]);
        let first = prefer_sticky_channel(None, &cands, "trace-abc-123");
        let second = prefer_sticky_channel(None, &cands, "trace-abc-123");
        assert_eq!(first, second);
        match first {
            Some(StickyDecision::Fallback { selected, reason }) => {
                assert!(cands.contains(&selected));
                assert_eq!(reason, StickyFallbackReason::NoHistory);
            }
            _ => panic!("expected Fallback (no history), got {first:?}"),
        }
    }

    // Mirrors Go `TestTraceStickyKeyProvider_AddKey_MinimalRemapping`: when
    // the historical sticky selection is still in the candidate set, it is
    // reused unchanged — adding new keys does NOT remap existing traces.
    #[test]
    fn prefer_sticky_channel_reuses_history_when_still_candidate() {
        let cands = candidates(&["key-1", "key-2", "key-3"]);
        // Pretend a prior call pinned "key-2" for this trace.
        let decision = prefer_sticky_channel(Some("key-2"), &cands, "trace-abc-123");
        match decision {
            Some(StickyDecision::Use { selected }) => assert_eq!(selected, "key-2"),
            _ => panic!("expected Use, got {decision:?}"),
        }
    }

    // Mirrors Go `TestTraceStickyKeyProvider_RemoveKey_MinimalRemapping` and
    // `TestTraceStickyKeyProvider_DisableKey_SimulatedByRemoval`: when the
    // sticky key is removed from the candidate set, fall back to a fresh
    // rendezvous pick over the remaining keys.
    #[test]
    fn prefer_sticky_channel_falls_back_when_history_removed() {
        let prior = candidates(&["key-1", "key-2", "key-3"]);
        // First, capture the historical selection.
        let historical = match prefer_sticky_channel(None, &prior, "trace-X") {
            Some(StickyDecision::Fallback { selected, .. }) => selected,
            other => panic!("expected Fallback, got {other:?}"),
        };

        // Now remove that key from the candidate set (simulating disablement).
        let reduced: Vec<String> = prior.into_iter().filter(|k| k != &historical).collect();
        // Sanity: the reduced set must not contain the historical key anymore.
        assert!(!reduced.contains(&historical));

        let decision = prefer_sticky_channel(Some(&historical), &reduced, "trace-X");
        match decision {
            Some(StickyDecision::Fallback { selected, reason }) => {
                assert!(reduced.contains(&selected));
                assert_ne!(selected, historical);
                assert_eq!(reason, StickyFallbackReason::RemovedFromCandidates);
            }
            other => panic!("expected Fallback, got {other:?}"),
        }
    }

    // Mirrors Go `TestTraceStickyKeyProvider_EmptyEnabledKeys_FallbackToFirst`:
    // with no candidates the pure decision is `None` — the caller must supply
    // the channel's full credentials list as the ultimate fallback (Go returns
    // `Credentials.APIKeys[0]`).
    #[test]
    fn prefer_sticky_channel_returns_none_when_no_candidates_and_no_history() {
        let empty: Vec<String> = Vec::new();
        assert_eq!(prefer_sticky_channel(None, &empty, "trace-1"), None);
    }

    // Single-candidate short-circuit: always that candidate, regardless of
    // history. Matches the Go `len(enabled) == 1` early return.
    #[test]
    fn prefer_sticky_channel_single_candidate_is_always_selected() {
        let one = candidates(&["only-key"]);
        // No history → Fallback with NoHistory reason.
        match prefer_sticky_channel(None, &one, "trace-1") {
            Some(StickyDecision::Fallback { selected, reason }) => {
                assert_eq!(selected, "only-key");
                assert_eq!(reason, StickyFallbackReason::NoHistory);
            }
            other => panic!("expected Fallback, got {other:?}"),
        }
        // Matching history → Use.
        match prefer_sticky_channel(Some("only-key"), &one, "trace-1") {
            Some(StickyDecision::Use { selected }) => assert_eq!(selected, "only-key"),
            other => panic!("expected Use, got {other:?}"),
        }
    }

    // Mirrors Go `TestTraceStickyKeyProvider_DifferentTraces_MaySelectDifferentKeys`:
    // across many distinct seeds, the rendezvous hash spreads selections over
    // more than one candidate (i.e. the hash is not degenerate). The canonical
    // FNV-1a 64-bit known-answer tests live in `channel_service` (which owns
    // the shared `hash_api_key_fnv64a` / `rendezvous_select` port).
    #[test]
    fn trace_sticky_select_distributes_across_candidates_over_many_seeds() {
        let cands = candidates(&["key-1", "key-2", "key-3", "key-4", "key-5"]);
        let mut picked = std::collections::HashSet::new();
        for i in 0..100u32 {
            let seed = format!("trace-{i}");
            let sel = trace_sticky_select(&cands, &seed);
            assert!(matches!(sel.as_deref(), Some(s) if cands.iter().any(|c| c == s)));
            if let Some(s) = sel {
                picked.insert(s);
            }
        }
        assert!(
            picked.len() > 1,
            "expected selections across >1 candidate, got {picked:?}"
        );
    }

    // Determinism: same (candidates, seed) → same pick.
    #[test]
    fn trace_sticky_select_is_deterministic() {
        let cands = candidates(&["alpha", "beta", "gamma"]);
        let a = trace_sticky_select(&cands, "seed-1");
        let b = trace_sticky_select(&cands, "seed-1");
        assert_eq!(a, b);
    }

    // =====================================================================
    // S16 — trace content/preview project-scoped authorization.
    // Mirrors Go `api/request_content.go::DownloadRequestContent` and
    // `api/request_live.go::PreviewRequest` cross-project guards.
    // =====================================================================

    #[test]
    fn authorize_trace_access_allows_same_project() {
        assert!(authorize_trace_access(7, 7).is_ok());
    }

    // Mirrors Go `if req.ProjectID != projectID { JSONError(404) }` — the
    // authorization primitive itself surfaces Forbidden; the HTTP layer maps
    // to 404 to avoid leaking existence.
    #[test]
    fn authorize_trace_access_denies_cross_project_as_forbidden() {
        match authorize_trace_access(7, 99) {
            Err(err) => {
                assert!(matches!(err.kind, conduit_core::ErrorKind::Forbidden));
                assert_eq!(err.http_status, 403);
            }
            Ok(()) => panic!("expected Err(Forbidden) for cross-project access"),
        }
    }

    #[test]
    fn authorize_trace_access_treats_zero_principal_as_unauthorized() {
        // A zero/empty principal project id cannot equal any real trace
        // project id (the Go handlers reject projectID<=0 up front with 400).
        match authorize_trace_access(0, 1) {
            Err(err) => {
                assert!(matches!(err.kind, conduit_core::ErrorKind::Forbidden));
            }
            Ok(()) => panic!("expected Err(Forbidden) for zero-principal access"),
        }
    }

    // =====================================================================
    // S17 — pure span tree-building logic. Mirrors Go
    // `internal/server/biz/trace.go` span-keying, dedup, prefix-match and
    // parent-finding functions (lines 1370-1566).
    //
    // Direct ports of the two Go pure-logic golden tests:
    //   * TestSpanToKey_CompactTypesIncludeSummary                (trace_test.go:673)
    //   * TestDeduplicateSpansWithParent_CompactSummaryUsesContentKey (trace_test.go:651)
    // Plus coverage of the S17 pure functions exercised only indirectly by
    // the DB-backed Go GetRootSegment tree tests (find_segment_parent,
    // count_common_span_prefix, extract_produced/consumed_tool_call_ids).
    //
    // pending DB-backed (ent client + transformer pipeline — NOT pure logic):
    //   * TestRequestService_LoadersReturnEmptyJSONAndSlices           (L84-145)
    //   * TestTraceService_GetOrCreateTrace                             (L147-186)
    //   * TestTraceService_GetOrCreateTrace_WithThread                  (L188-219)
    //   * TestTraceService_GetOrCreateTrace_DifferentProjects           (L221-258)
    //   * TestTraceService_GetTraceByID                                  (L260-295)
    //   * TestTraceService_GetRequestTrace                              (L297-385)
    //   * TestTraceService_GetRequestTrace_WithToolCalls                (L387-481)
    //   * TestTraceService_GetRequestTrace_AnthropicResponseTransformation (L483-572)
    //   * TestTraceService_GetRequestTrace_WithReasoningContent         (L574-649)
    //   * TestTraceService_GetRequestTrace_EmptyTrace                   (L708-735)
    //   * TestTraceService_GetRequestTrace_MultipleRequestsWithToolResults (L737-887)
    //   * TestTraceService_GetRootSegment_TreeByToolCallID              (L889-1052)
    //   * TestTraceService_GetRootSegment_TreeBySpanPrefixMatch         (L1054-1167)
    //   * TestTraceService_GetRootSegment_FallbackChronologicalNearest  (L1169-1421)
    //   * TestTraceService_GetRootSegment_CrossTraceDedup              (L1423-1913)
    //   * TestTraceService_GetRequestTrace_integration                  (L1915-1936)
    // These require the ent ORM + transformer pipeline (requestToSegment
    // body→spans extraction) which is not yet ported. (Hilbert-the-14th)
    // =====================================================================

    fn fixed_ts(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(secs, 0)
            .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_else(Utc::now))
    }

    fn make_span(id: &str, span_type: &str, value: Option<SpanValue>) -> Span {
        Span {
            id: id.to_owned(),
            r#type: span_type.to_owned(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            value,
        }
    }

    // Mirrors Go TestSpanToKey_CompactTypesIncludeSummary: "compaction" case
    // (trace_test.go:679-688).
    #[test]
    fn s17_span_to_key_compaction_includes_summary() {
        let span = make_span(
            "s1",
            "compaction",
            Some(SpanValue {
                compaction: Some(SpanCompaction {
                    summary: "compact-a".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&span), "compaction:compact-a");
    }

    // Mirrors Go TestSpanToKey_CompactTypesIncludeSummary: "compaction_summary" case
    // (trace_test.go:689-698).
    #[test]
    fn s17_span_to_key_compaction_summary_includes_summary() {
        let span = make_span(
            "s1",
            "compaction_summary",
            Some(SpanValue {
                compaction: Some(SpanCompaction {
                    summary: "compact-b".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&span), "compaction_summary:compact-b");
    }

    // Comprehensive: every branch of Go spanToKey (trace.go:1499-1566).
    // Each assertion mirrors the corresponding Go `case` in the switch.
    #[test]
    fn s17_span_to_key_for_every_span_value_type() {
        // user_query
        let s = make_span(
            "s",
            "user_query",
            Some(SpanValue {
                user_query: Some(SpanUserQuery {
                    text: "hello".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "user_query:hello");

        // user_image_url
        let s = make_span(
            "s",
            "user_image_url",
            Some(SpanValue {
                user_image_url: Some(SpanUserImageURL {
                    url: "http://img".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "user_image_url:http://img");

        // user_video_url
        let s = make_span(
            "s",
            "user_video_url",
            Some(SpanValue {
                user_video_url: Some(SpanUserVideoURL {
                    url: "http://vid".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "user_video_url:http://vid");

        // user_input_audio
        let s = make_span(
            "s",
            "user_input_audio",
            Some(SpanValue {
                user_input_audio: Some(SpanUserInputAudio {
                    format: "wav".into(),
                    data: "base64data".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "user_input_audio:wav:base64data");

        // text
        let s = make_span(
            "s",
            "text",
            Some(SpanValue {
                text: Some(SpanText {
                    text: "response".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "text:response");

        // thinking
        let s = make_span(
            "s",
            "thinking",
            Some(SpanValue {
                thinking: Some(SpanThinking {
                    thinking: "hmm".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "thinking:hmm");

        // image_url
        let s = make_span(
            "s",
            "image_url",
            Some(SpanValue {
                image_url: Some(SpanImageURL {
                    url: "http://i".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "image_url:http://i");

        // video_url
        let s = make_span(
            "s",
            "video_url",
            Some(SpanValue {
                video_url: Some(SpanVideoURL {
                    url: "http://v".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "video_url:http://v");

        // audio: <type>:<id>:<format>:<transcript>
        let s = make_span(
            "s",
            "audio",
            Some(SpanValue {
                audio: Some(SpanAudio {
                    id: "aid".into(),
                    format: "mp3".into(),
                    data: String::new(),
                    transcript: "hello".into(),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "audio:aid:mp3:hello");

        // tool_use with arguments
        let s = make_span(
            "s",
            "tool_use",
            Some(SpanValue {
                tool_use: Some(SpanToolUse {
                    id: "call_1".into(),
                    r#type: "function".into(),
                    name: "get_weather".into(),
                    arguments: Some(r#"{"city":"SF"}"#.into()),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(
            span_to_key(&s),
            r#"tool_use:call_1:get_weather:{"city":"SF"}"#
        );

        // tool_use without arguments (None → empty string)
        let s = make_span(
            "s",
            "tool_use",
            Some(SpanValue {
                tool_use: Some(SpanToolUse {
                    id: "call_2".into(),
                    r#type: "function".into(),
                    name: "search".into(),
                    arguments: None,
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "tool_use:call_2:search:");

        // tool_result with text, is_error=false
        let s = make_span(
            "s",
            "tool_result",
            Some(SpanValue {
                tool_result: Some(SpanToolResult {
                    tool_call_id: "call_1".into(),
                    is_error: false,
                    text: Some("72F sunny".into()),
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "tool_result:call_1:false:72F sunny");

        // tool_result error flag, no text
        let s = make_span(
            "s",
            "tool_result",
            Some(SpanValue {
                tool_result: Some(SpanToolResult {
                    tool_call_id: "call_e".into(),
                    is_error: true,
                    text: None,
                }),
                ..Default::default()
            }),
        );
        assert_eq!(span_to_key(&s), "tool_result:call_e:true:");
    }

    // Go spanToKey: `if span.Value == nil { return "<type>:" }`.
    #[test]
    fn s17_span_to_key_with_no_value_collapses_to_type_colon() {
        let s = make_span("s1", "text", None);
        assert_eq!(span_to_key(&s), "text:");
    }

    // Go spanToKey has NO case for "system_instruction" → falls through to the
    // default `fmt.Sprintf("%s:", span.Type)`, ignoring content entirely.
    // This is why CrossTraceDedup strips a prior trace's system_instruction
    // regardless of its text — the keys are identical. (trace_test.go:1527-1530)
    #[test]
    fn s17_span_to_key_system_instruction_ignores_content() {
        let s1 = make_span(
            "s1",
            "system_instruction",
            Some(SpanValue {
                system_instruction: Some(SpanSystemInstruction {
                    instruction: "Be helpful".into(),
                }),
                ..Default::default()
            }),
        );
        let s2 = make_span(
            "s2",
            "system_instruction",
            Some(SpanValue {
                system_instruction: Some(SpanSystemInstruction {
                    instruction: "Be rude".into(),
                }),
                ..Default::default()
            }),
        );
        // Both collapse to "system_instruction:" — content is ignored.
        assert_eq!(span_to_key(&s1), "system_instruction:");
        assert_eq!(span_to_key(&s1), span_to_key(&s2));
    }

    // Mirrors Go TestDeduplicateSpansWithParent_CompactSummaryUsesContentKey
    // (trace_test.go:651): a child compaction with a different summary is NOT
    // deduped because spanToKey includes the summary content.
    #[test]
    fn s17_deduplicate_compact_summary_uses_content_key() {
        let parent = vec![make_span(
            "parent-compact",
            "compaction",
            Some(SpanValue {
                compaction: Some(SpanCompaction {
                    summary: "summary-a".into(),
                }),
                ..Default::default()
            }),
        )];
        let current = vec![make_span(
            "child-compact",
            "compaction",
            Some(SpanValue {
                compaction: Some(SpanCompaction {
                    summary: "summary-b".into(),
                }),
                ..Default::default()
            }),
        )];

        let result = deduplicate_spans_with_parent(&current, &parent);
        assert_eq!(result.len(), 1);
        let summary = result[0]
            .value
            .as_ref()
            .and_then(|v| v.compaction.as_ref())
            .map(|c| c.summary.as_str());
        assert_eq!(summary, Some("summary-b"));
    }

    #[test]
    fn s17_deduplicate_empty_current_returns_empty() {
        let parent = vec![make_span("p1", "text", None)];
        let result = deduplicate_spans_with_parent(&[], &parent);
        assert!(result.is_empty());
    }

    #[test]
    fn s17_deduplicate_empty_parent_returns_current_unchanged() {
        let current = vec![make_span("c1", "text", None), make_span("c2", "text", None)];
        let result = deduplicate_spans_with_parent(&current, &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn s17_deduplicate_all_match_strips_everything() {
        let parent = vec![
            make_span(
                "p1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: "q".into() }),
                    ..Default::default()
                }),
            ),
            make_span(
                "p2",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "a".into() }),
                    ..Default::default()
                }),
            ),
        ];
        let current = vec![
            make_span(
                "c1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: "q".into() }),
                    ..Default::default()
                }),
            ),
            make_span(
                "c2",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "a".into() }),
                    ..Default::default()
                }),
            ),
        ];
        let result = deduplicate_spans_with_parent(&current, &parent);
        assert!(result.is_empty());
    }

    // Mirrors the dedup behavior behind TreeByToolCallID (trace_test.go:1048-1051):
    // the shared prefix (user_query + tool_calls) is stripped, only the unique
    // tool_result span remains.
    #[test]
    fn s17_deduplicate_strips_matching_prefix_only() {
        let parent = vec![make_span(
            "p1",
            "user_query",
            Some(SpanValue {
                user_query: Some(SpanUserQuery {
                    text: "same".into(),
                }),
                ..Default::default()
            }),
        )];
        let current = vec![
            make_span(
                "c1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "same".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "c2",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: "new".into() }),
                    ..Default::default()
                }),
            ),
        ];
        let result = deduplicate_spans_with_parent(&current, &parent);
        // First matches (stripped), second exceeds parent len (kept).
        assert_eq!(result.len(), 1);
        let text = result[0]
            .value
            .as_ref()
            .and_then(|v| v.user_query.as_ref())
            .map(|q| q.text.as_str());
        assert_eq!(text, Some("new"));
    }

    // Mirrors CrossTraceDedup scenario (trace_test.go:1527-1530): system_instruction
    // spans are content-agnostic in spanToKey, so the parent's system_instruction
    // strips the child's system_instruction regardless of content.
    #[test]
    fn s17_deduplicate_system_instruction_is_content_agnostic() {
        let parent = vec![make_span(
            "p1",
            "system_instruction",
            Some(SpanValue {
                system_instruction: Some(SpanSystemInstruction {
                    instruction: "Be helpful".into(),
                }),
                ..Default::default()
            }),
        )];
        let current = vec![make_span(
            "c1",
            "system_instruction",
            Some(SpanValue {
                system_instruction: Some(SpanSystemInstruction {
                    instruction: "Be rude".into(),
                }),
                ..Default::default()
            }),
        )];
        let result = deduplicate_spans_with_parent(&current, &parent);
        // Despite different instructions, keys match → deduped.
        assert!(result.is_empty());
    }

    #[test]
    fn s17_count_common_prefix_identical_spans() {
        let spans = vec![
            make_span(
                "s1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: "q".into() }),
                    ..Default::default()
                }),
            ),
            make_span(
                "s2",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "a".into() }),
                    ..Default::default()
                }),
            ),
        ];
        assert_eq!(count_common_span_prefix(&spans, &spans), 2);
    }

    #[test]
    fn s17_count_common_prefix_stops_at_mismatch() {
        let a = vec![
            make_span(
                "s1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "same".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "s2",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "a".into() }),
                    ..Default::default()
                }),
            ),
        ];
        let b = vec![
            make_span(
                "s1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "same".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "s2",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "b".into() }),
                    ..Default::default()
                }),
            ),
        ];
        assert_eq!(count_common_span_prefix(&a, &b), 1);
    }

    #[test]
    fn s17_count_common_prefix_empty_returns_zero() {
        let spans = vec![make_span("s1", "text", None)];
        assert_eq!(count_common_span_prefix(&[], &spans), 0);
        assert_eq!(count_common_span_prefix(&spans, &[]), 0);
        assert_eq!(count_common_span_prefix(&[], &[]), 0);
    }

    #[test]
    fn s17_extract_produced_tool_call_ids_from_response() {
        let response = vec![
            make_span(
                "t1",
                "tool_use",
                Some(SpanValue {
                    tool_use: Some(SpanToolUse {
                        id: "call_A".into(),
                        r#type: "function".into(),
                        name: "task_a".into(),
                        arguments: None,
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "t2",
                "tool_use",
                Some(SpanValue {
                    tool_use: Some(SpanToolUse {
                        id: "call_B".into(),
                        r#type: "function".into(),
                        name: "task_b".into(),
                        arguments: None,
                    }),
                    ..Default::default()
                }),
            ),
            // Non-tool_use span should be ignored.
            make_span(
                "txt",
                "text",
                Some(SpanValue {
                    text: Some(SpanText { text: "hi".into() }),
                    ..Default::default()
                }),
            ),
            // Empty id should be skipped (Go guard: ToolUse.ID != "").
            make_span(
                "t3",
                "tool_use",
                Some(SpanValue {
                    tool_use: Some(SpanToolUse {
                        id: String::new(),
                        r#type: "function".into(),
                        name: "noop".into(),
                        arguments: None,
                    }),
                    ..Default::default()
                }),
            ),
        ];
        let ids = extract_produced_tool_call_ids(&response);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("call_A"));
        assert!(ids.contains("call_B"));
    }

    #[test]
    fn s17_extract_consumed_tool_call_ids_from_request() {
        let request = vec![
            make_span(
                "tr1",
                "tool_result",
                Some(SpanValue {
                    tool_result: Some(SpanToolResult {
                        tool_call_id: "call_A".into(),
                        is_error: false,
                        text: Some("done".into()),
                    }),
                    ..Default::default()
                }),
            ),
            // Non-tool_result span should be ignored.
            make_span(
                "uq1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: "q".into() }),
                    ..Default::default()
                }),
            ),
            // Empty tool_call_id should be skipped (Go guard: ToolCallID != "").
            make_span(
                "tr2",
                "tool_result",
                Some(SpanValue {
                    tool_result: Some(SpanToolResult {
                        tool_call_id: String::new(),
                        is_error: false,
                        text: None,
                    }),
                    ..Default::default()
                }),
            ),
        ];
        let ids = extract_consumed_tool_call_ids(&request);
        assert_eq!(ids.len(), 1);
        assert!(ids.contains("call_A"));
    }

    #[test]
    fn s17_segment_build_info_combines_spans_and_extracts_ids() {
        let request = vec![make_span(
            "tr1",
            "tool_result",
            Some(SpanValue {
                tool_result: Some(SpanToolResult {
                    tool_call_id: "call_X".into(),
                    is_error: false,
                    text: Some("result".into()),
                }),
                ..Default::default()
            }),
        )];
        let response = vec![make_span(
            "tu1",
            "tool_use",
            Some(SpanValue {
                tool_use: Some(SpanToolUse {
                    id: "call_Y".into(),
                    r#type: "function".into(),
                    name: "next_call".into(),
                    arguments: None,
                }),
                ..Default::default()
            }),
        )];
        let info = SegmentBuildInfo::from_spans("seg-1", fixed_ts(100), &request, &response);
        assert_eq!(info.segment_id, "seg-1");
        assert_eq!(info.origin_spans.len(), 2);
        assert_eq!(info.origin_request_spans.len(), 1);
        assert!(info.produced_tool_call_ids.contains("call_Y"));
        assert!(info.consumed_tool_call_ids.contains("call_X"));
    }

    // --- find_segment_parent: 3-tier strategy ---

    // Strategy 1: tool_call_id matching — mirrors Go TreeByToolCallID
    // (trace_test.go:889-1052). A child consuming tool_call_ids from a
    // predecessor's response links to that predecessor. When current consumes
    // ids from MULTIPLE predecessors, the LATEST (by segment_start_time) wins.
    #[test]
    fn s17_find_parent_tool_call_id_match_picks_latest_producer() {
        let pred1 = SegmentBuildInfo::from_spans(
            "seg-1",
            fixed_ts(100),
            &[],
            &[make_span(
                "tu1",
                "tool_use",
                Some(SpanValue {
                    tool_use: Some(SpanToolUse {
                        id: "call_A".into(),
                        r#type: "function".into(),
                        name: "task_a".into(),
                        arguments: None,
                    }),
                    ..Default::default()
                }),
            )],
        );
        let pred2 = SegmentBuildInfo::from_spans(
            "seg-2",
            fixed_ts(200),
            &[],
            &[make_span(
                "tu2",
                "tool_use",
                Some(SpanValue {
                    tool_use: Some(SpanToolUse {
                        id: "call_B".into(),
                        r#type: "function".into(),
                        name: "task_b".into(),
                        arguments: None,
                    }),
                    ..Default::default()
                }),
            )],
        );
        let predecessors = vec![pred1, pred2];

        let mut tool_call_index = std::collections::BTreeMap::new();
        tool_call_index.insert("call_A".to_string(), 0);
        tool_call_index.insert("call_B".to_string(), 1);

        let current = SegmentBuildInfo::from_spans(
            "seg-3",
            fixed_ts(300),
            &[
                make_span(
                    "tr1",
                    "tool_result",
                    Some(SpanValue {
                        tool_result: Some(SpanToolResult {
                            tool_call_id: "call_A".into(),
                            is_error: false,
                            text: Some("A done".into()),
                        }),
                        ..Default::default()
                    }),
                ),
                make_span(
                    "tr2",
                    "tool_result",
                    Some(SpanValue {
                        tool_result: Some(SpanToolResult {
                            tool_call_id: "call_B".into(),
                            is_error: false,
                            text: Some("B done".into()),
                        }),
                        ..Default::default()
                    }),
                ),
            ],
            &[],
        );

        let parent = find_segment_parent(&current, &predecessors, &tool_call_index);
        // Both pred1 and pred2 produced consumed ids; pred2 is later → wins.
        assert_eq!(parent.map(|p| p.segment_id.as_str()), Some("seg-2"));
    }

    // Strategy 2: span prefix matching — mirrors Go TreeBySpanPrefixMatch
    // (trace_test.go:1054-1167). The predecessor with the longest common
    // request-span prefix wins.
    #[test]
    fn s17_find_parent_span_prefix_match_picks_longest() {
        let shared_req = vec![make_span(
            "u1",
            "user_query",
            Some(SpanValue {
                user_query: Some(SpanUserQuery {
                    text: "Hello".into(),
                }),
                ..Default::default()
            }),
        )];
        let pred1 = SegmentBuildInfo::from_spans("seg-1", fixed_ts(100), &shared_req, &[]);

        // pred2 has a longer origin_spans that includes pred1's prefix + more.
        let pred2_full = vec![
            make_span(
                "u1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "Hello".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "t1",
                "text",
                Some(SpanValue {
                    text: Some(SpanText {
                        text: "Hi there!".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "u2",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "Tell me more".into(),
                    }),
                    ..Default::default()
                }),
            ),
        ];
        let pred2 = SegmentBuildInfo {
            segment_id: "seg-2".into(),
            segment_start_time: fixed_ts(200),
            origin_spans: pred2_full.clone(),
            origin_request_spans: pred2_full,
            produced_tool_call_ids: Default::default(),
            consumed_tool_call_ids: Default::default(),
        };

        // Current carries all of pred2's spans + one new → prefix with pred2 is longer.
        let current_req = vec![
            make_span(
                "u1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "Hello".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "t1",
                "text",
                Some(SpanValue {
                    text: Some(SpanText {
                        text: "Hi there!".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "u2",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "Tell me more".into(),
                    }),
                    ..Default::default()
                }),
            ),
            make_span(
                "u3",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery {
                        text: "Thanks!".into(),
                    }),
                    ..Default::default()
                }),
            ),
        ];
        let current = SegmentBuildInfo {
            segment_id: "seg-3".into(),
            segment_start_time: fixed_ts(300),
            origin_spans: current_req.clone(),
            origin_request_spans: current_req,
            produced_tool_call_ids: Default::default(),
            consumed_tool_call_ids: Default::default(),
        };

        let predecessors = vec![pred1, pred2];
        let tool_call_index = std::collections::BTreeMap::new();
        let parent = find_segment_parent(&current, &predecessors, &tool_call_index);
        // pred2 shares 3 prefix spans vs pred1's 1 → pred2 wins.
        assert_eq!(parent.map(|p| p.segment_id.as_str()), Some("seg-2"));
    }

    // Strategy 2 tie: Go replaces bestMatch only on strictly greater matchLen
    // (`if matchLen > bestMatchLen`), so the FIRST predecessor with the best
    // length wins.
    #[test]
    fn s17_find_parent_prefix_tie_resolves_to_first() {
        let req = vec![make_span(
            "u1",
            "user_query",
            Some(SpanValue {
                user_query: Some(SpanUserQuery {
                    text: "same".into(),
                }),
                ..Default::default()
            }),
        )];
        let make_seg = |id: &str, t: i64| SegmentBuildInfo {
            segment_id: id.into(),
            segment_start_time: fixed_ts(t),
            origin_spans: req.clone(),
            origin_request_spans: req.clone(),
            produced_tool_call_ids: Default::default(),
            consumed_tool_call_ids: Default::default(),
        };
        let predecessors = vec![make_seg("first", 100), make_seg("second", 200)];
        let current = SegmentBuildInfo {
            segment_id: "cur".into(),
            segment_start_time: fixed_ts(300),
            origin_spans: req.clone(),
            origin_request_spans: req,
            produced_tool_call_ids: Default::default(),
            consumed_tool_call_ids: Default::default(),
        };
        let tool_call_index = std::collections::BTreeMap::new();
        let parent = find_segment_parent(&current, &predecessors, &tool_call_index);
        // Tie → first predecessor wins.
        assert_eq!(parent.map(|p| p.segment_id.as_str()), Some("first"));
    }

    // Strategy 3: fallback — mirrors Go FallbackChronologicalNearest
    // (trace_test.go:1169-1421). Disjoint requests (no tool_call_ids, no
    // prefix match) chain to the nearest predecessor (the last entry).
    #[test]
    fn s17_find_parent_fallback_nearest_predecessor() {
        let make_seg = |id: &str, t: i64, text: &str| {
            let spans = vec![make_span(
                "u1",
                "user_query",
                Some(SpanValue {
                    user_query: Some(SpanUserQuery { text: text.into() }),
                    ..Default::default()
                }),
            )];
            SegmentBuildInfo {
                segment_id: id.into(),
                segment_start_time: fixed_ts(t),
                origin_spans: spans.clone(),
                origin_request_spans: spans,
                produced_tool_call_ids: Default::default(),
                consumed_tool_call_ids: Default::default(),
            }
        };
        let predecessors = vec![
            make_seg("seg-1", 100, "Alpha"),
            make_seg("seg-2", 200, "Bravo"),
        ];
        let current = make_seg("cur", 300, "Charlie");
        let tool_call_index = std::collections::BTreeMap::new();
        let parent = find_segment_parent(&current, &predecessors, &tool_call_index);
        // No tool_call match, no prefix match → fallback to last predecessor.
        assert_eq!(parent.map(|p| p.segment_id.as_str()), Some("seg-2"));
    }

    #[test]
    fn s17_find_parent_empty_predecessors_returns_none() {
        let current = SegmentBuildInfo::from_spans("seg-1", fixed_ts(100), &[], &[]);
        let predecessors: Vec<SegmentBuildInfo> = vec![];
        let tool_call_index = std::collections::BTreeMap::new();
        assert!(find_segment_parent(&current, &predecessors, &tool_call_index).is_none());
    }
}

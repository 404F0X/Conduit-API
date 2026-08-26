//! ADPT-REQUEST-EXECUTIONS — host adapter wiring the admin GraphQL
//! `RequestExecution` connection query domain to the real configured repository.
//!
//! Implements the host-injected service seam
//! [`conduit_admin_graphql::request_execution::RequestExecutionQueryServices`]
//! backed by [`RequestExecutionRepo`]. The Leader wires `mod` +
//! `.data(Arc<dyn RequestExecutionQueryServices>)` centrally — this file is
//! self-contained and touches no other module.
//!
//! ## Go parity anchors
//!
//! - The Go snapshot has **no top-level `Query.requestExecutions`** — the type
//!   is reachable only through edge fields (`Request.executions`,
//!   `Channel.executions`, `DataStorage.executions`), each of which injects an
//!   implicit FK predicate (`where: { requestID: ... }` etc.) into the
//!   ent-generated connection. This adapter therefore serves arbitrary
//!   `RequestExecutionConnectionArgs` and honors the injected ID predicates.
//! - Ordering: ent remaps `CREATED_AT` to the default ID order
//!   (`RequestExecutionOrderTerm::Id`, see `resolve_request_execution_order`);
//!   `UPDATED_AT` sorts on `updated_at`. When no order is supplied the adapter
//!   defaults to CREATED_AT DESC (newest first — matches the admin UI and the
//!   `wiring_requests.rs` template).
//! - Pagination: bounded materialization (repo loads up to
//!   [`EXECUTION_LOAD_LIMIT`] rows — admin-scale table), then Relay forward
//!   pagination in-memory with absolute-index cursors, same strategy as
//!   `wiring_requests.rs`. `last`/`before` (backward pagination) are DEFERRED —
//!   the frontend only paginates forward.
//!
//! ## Where-filter coverage
//!
//! Implemented predicate families (ent SQL semantics — predicates on a
//! nullable column never match a NULL row):
//!   - combinators: `not` / `and` / `or`
//!   - `id`, `requestID`, `channelID`, `dataStorageID`: eq/NEQ/In/NotIn
//!     (+ IsNil/NotNil for the nullable FKs); ID values accept both raw ids
//!     and `gid://conduit/<Type>/<id>` global ids
//!   - `projectID`, `responseStatusCode`: eq/NEQ/In/NotIn/GT/GTE/LT/LTE
//!     (+ IsNil/NotNil for the nullable status code)
//!   - `createdAt`, `updatedAt`: eq/NEQ/GT/GTE/LT/LTE
//!   - `status`: eq/NEQ/In/NotIn; `stream`, `passThroughApplied`: eq/NEQ
//!   - `modelID`, `format`: eq/NEQ/In/NotIn/Contains/HasPrefix/HasSuffix
//!   - `externalID`, `errorMessage`: eq/Contains/IsNil/NotNil
//!
//! DEFERRED (silently ignored — same policy as the deferred complex
//! predicates in `wiring_requests.rs`):
//!   - lexicographic string ordering (GT/GTE/LT/LTE on string fields) — DB
//!     collation vs Rust byte-order mismatch risk, no frontend consumer;
//!   - case-fold variants (`*EqualFold` / `*ContainsFold`) — Go uses SQL
//!     LOWER() folding, no frontend consumer;
//!   - `metricsLatencyMs*` / `metricsFirstTokenLatencyMs*` /
//!     `metricsReasoningDurationMs*` families — latency filtering is not
//!     exposed in the admin UI;
//!   - `requestURL*` family, remaining `externalID`/`errorMessage` variants,
//!     and `createdAtIn`/`NotIn` (+ updatedAt equivalents) — no consumer;
//!   - edge predicates (`hasRequestWith` etc.) are not in the input type yet
//!     (pending on the graphql side).

use std::sync::Arc;

use async_graphql::ID;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use conduit_admin_graphql::pagination::PageInfo;
use conduit_admin_graphql::request_execution::{
    OrderDirection, RequestExecution, RequestExecutionConnection, RequestExecutionConnectionArgs,
    RequestExecutionEdge, RequestExecutionOrderSelection, RequestExecutionOrderTerm,
    RequestExecutionQueryError, RequestExecutionQueryServices, RequestExecutionStatus,
    RequestExecutionWhereInput,
};
use conduit_admin_graphql::scalars::{CursorScalar, JsonRawMessageScalar, TimeScalar};
use conduit_db::repo::request_execution_repo::RequestExecutionRepo;
use conduit_db::row::RequestExecutionRow;

// ---------------------------------------------------------------------------
// Row loading
// ---------------------------------------------------------------------------

/// Bounded-materialization cap — same admin-scale bound as
/// `wiring_requests.rs` (`limit: 1000`).
const EXECUTION_LOAD_LIMIT: i64 = 1000;

/// SELECT clause mirroring the repo's private `REQUEST_EXECUTION_SELECT_COLUMNS`
/// (`request-execution repository`): INTEGER id/edge columns stringified
/// via CAST, nullable JSON columns COALESCEd to the JSON text `null` so
// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing [`RequestExecutionQueryServices`] adapter backed by the live
/// [`RequestExecutionRepo`].
pub struct RequestExecutionAdapter {
    repo: Arc<dyn RequestExecutionRepo>,
    data_storage_repo: Option<Arc<dyn conduit_db::DataStorageRepo>>,
}

impl RequestExecutionAdapter {
    pub fn new(repo: Arc<dyn RequestExecutionRepo>) -> Self {
        Self {
            repo,
            data_storage_repo: None,
        }
    }

    pub fn with_data_storage_repo(mut self, repo: Arc<dyn conduit_db::DataStorageRepo>) -> Self {
        self.data_storage_repo = Some(repo);
        self
    }
}

#[async_trait]
impl RequestExecutionQueryServices for RequestExecutionAdapter {
    async fn request_executions(
        &self,
        args: RequestExecutionConnectionArgs,
    ) -> Result<RequestExecutionConnection, RequestExecutionQueryError> {
        // Request edge resolvers inject both requestID and projectID. Use the
        // repository's indexed request-scoped query for that hot path; loading
        // the oldest EXECUTION_LOAD_LIMIT rows globally before filtering made
        // executions disappear once the table exceeded the cap and put
        // avoidable pressure on the PostgreSQL pool. Non-edge callers retain
        // the bounded global fallback. Remaining predicates, ordering and
        // Relay pagination are still applied below.
        let ctx = conduit_db::RequestContext::new(conduit_db::PolicyContext::new(
            conduit_db::Principal::test(),
        ));
        let request_scope = args.where_filter.as_ref().and_then(|filter| {
            let request_id = filter.request_id.as_ref()?;
            let project_id = filter.project_id?;
            Some((project_id.to_string(), raw_id(request_id).to_string()))
        });
        let mut rows: Vec<RequestExecutionRow> = match request_scope {
            Some((project_id, request_id)) => {
                self.repo
                    .list_request_executions_unchecked(&ctx, &project_id, &request_id)
                    .await
            }
            None => {
                self.repo
                    .list_all_request_executions_unchecked(&ctx, EXECUTION_LOAD_LIMIT as u32)
                    .await
            }
        }
        .map_err(|e| RequestExecutionQueryError::Query(e.to_string()))?;

        // Where-filter (includes the FK predicates injected by the
        // `Request.executions` / `Channel.executions` edge resolvers).
        if let Some(filter) = &args.where_filter {
            rows.retain(|row| row_matches(filter, row));
        }

        // Sort. Ent remaps CREATED_AT to the default ID order (`Id` term);
        // default when no order is supplied: newest first (CREATED_AT DESC).
        let order = args.order_by.unwrap_or(RequestExecutionOrderSelection {
            direction: OrderDirection::Desc,
            term: RequestExecutionOrderTerm::Id,
        });
        sort_rows(&mut rows, order);

        // Relay forward pagination with absolute-index cursors.
        let (mut page, total_count, has_previous_page, has_next_page) =
            paginate_rows(rows, args.first, args.after.as_deref());

        if let Some(storage_repo) = self.data_storage_repo.as_ref() {
            for (_, row) in &mut page {
                crate::wiring_request_content::hydrate_execution_artifacts(
                    storage_repo.as_ref(),
                    row,
                )
                .await;
            }
        }

        let edges: Vec<Option<RequestExecutionEdge>> = page
            .into_iter()
            .map(|(idx, row)| {
                Some(RequestExecutionEdge {
                    cursor: CursorScalar(idx.to_string()),
                    node: Some(execution_row_to_gql(row)),
                })
            })
            .collect();

        let start_cursor = edges
            .first()
            .and_then(|e| e.as_ref())
            .map(|e| e.cursor.clone());
        let end_cursor = edges
            .last()
            .and_then(|e| e.as_ref())
            .map(|e| e.cursor.clone());

        Ok(RequestExecutionConnection {
            edges: Some(edges),
            page_info: PageInfo {
                has_next_page,
                has_previous_page,
                start_cursor,
                end_cursor,
            },
            total_count,
        })
    }
}

// ---------------------------------------------------------------------------
// Row → GraphQL node conversion
// ---------------------------------------------------------------------------

/// Convert a [`RequestExecutionRow`] to the GraphQL [`RequestExecution`]
/// shape. IDs are encoded as `gid://conduit/<Type>/<id>` global ids (same
/// convention as `wiring_requests.rs`).
fn execution_row_to_gql(row: RequestExecutionRow) -> RequestExecution {
    // `response_chunks` is stored as one JSON array (Go
    // `[]objects.JSONRawMessage`); the node wants a list of raw JSON values.
    // A non-array value (including JSON null) maps to the GraphQL null list.
    let response_chunks: Option<Vec<JsonRawMessageScalar>> =
        row.response_chunks.and_then(|value| match value {
            Value::Array(items) => Some(items.into_iter().map(JsonRawMessageScalar).collect()),
            _ => None,
        });

    RequestExecution {
        id: format!("gid://conduit/RequestExecution/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        // Go `field.Int("project_id").Default(1)` — the row always carries an
        // integer string; fall back to the Go default on the impossible path.
        project_id: row.project_id.parse().unwrap_or(1),
        request_id: format!("gid://conduit/Request/{}", row.request_id).into(),
        channel_id: row
            .channel_id
            .map(|id| format!("gid://conduit/Channel/{id}").into()),
        data_storage_id: row
            .data_storage_id
            .map(|id| format!("gid://conduit/DataStorage/{id}").into()),
        external_id: row.external_id,
        model_id: row.model_id,
        format: row.format,
        request_body: JsonRawMessageScalar(row.request_body),
        response_body: row.response_body.map(JsonRawMessageScalar),
        response_chunks,
        error_message: row.error_message,
        response_status_code: row.response_status_code,
        status: status_from_str(&row.status),
        stream: row.stream,
        metrics_latency_ms: row.metrics_latency_ms,
        metrics_first_token_latency_ms: row.metrics_first_token_latency_ms,
        metrics_reasoning_duration_ms: row.metrics_reasoning_duration_ms,
        request_headers: row.request_headers.map(JsonRawMessageScalar),
        request_url: row.request_url,
        pass_through_applied: row.pass_through_applied,
    }
}

/// Row status string → GraphQL enum (Go `requestexecution.Status`:
/// pending|processing|completed|failed|canceled). Unknown strings cannot be
/// produced by the repo; map them to `pending` like
/// `request_status_from_str` in `wiring_requests.rs`.
fn status_from_str(s: &str) -> RequestExecutionStatus {
    match s {
        "processing" => RequestExecutionStatus::Processing,
        "completed" => RequestExecutionStatus::Completed,
        "failed" => RequestExecutionStatus::Failed,
        "canceled" => RequestExecutionStatus::Canceled,
        _ => RequestExecutionStatus::Pending,
    }
}

/// GraphQL enum → row status string (inverse of [`status_from_str`]).
fn status_to_str(status: RequestExecutionStatus) -> &'static str {
    match status {
        RequestExecutionStatus::Pending => "pending",
        RequestExecutionStatus::Processing => "processing",
        RequestExecutionStatus::Completed => "completed",
        RequestExecutionStatus::Failed => "failed",
        RequestExecutionStatus::Canceled => "canceled",
    }
}

// ---------------------------------------------------------------------------
// Sorting
// ---------------------------------------------------------------------------

/// Numeric id for stable tie-breaking (row ids are integer strings — the
/// database table is integer-keyed, mirroring Go Ent).
fn row_id_num(row: &RequestExecutionRow) -> i64 {
    row.id.parse().unwrap_or(0)
}

/// Sort rows per the resolved order selection. `Id` is the ent remap of
/// CREATED_AT (gql_pagination.go:413) — sort key `(created_at, id)`;
/// `UpdatedAt` sorts on `(updated_at, id)`. Desc reverses the whole order
/// including the tie-breaker, matching `ORDER BY ... DESC, id DESC`.
fn sort_rows(rows: &mut [RequestExecutionRow], order: RequestExecutionOrderSelection) {
    match order.term {
        RequestExecutionOrderTerm::Id => {
            rows.sort_by_key(|row| (row.created_at, row_id_num(row)));
        }
        RequestExecutionOrderTerm::UpdatedAt => {
            rows.sort_by_key(|row| (row.updated_at, row_id_num(row)));
        }
    }
    if matches!(order.direction, OrderDirection::Desc) {
        rows.reverse();
    }
}

// ---------------------------------------------------------------------------
// Pagination (forward-only, absolute-index cursors)
// ---------------------------------------------------------------------------

/// Slice the filtered+sorted row list per `first`/`after`. Cursors are the
/// absolute index of the row within the filtered+sorted list (same scheme as
/// `wiring_requests.rs`, with `after` actually honored). Returns
/// `(indexed page, total_count, has_previous_page, has_next_page)`.
fn paginate_rows(
    rows: Vec<RequestExecutionRow>,
    first: Option<i32>,
    after: Option<&str>,
) -> (Vec<(usize, RequestExecutionRow)>, i64, bool, bool) {
    let total = rows.len() as i64;
    // Default page size 20, clamped to 1..=100 (wiring_requests.rs bounds).
    let limit = first.unwrap_or(20).clamp(1, 100) as usize;
    // `after` is exclusive: start just past the cursor index. An unparseable
    // cursor starts from the beginning (forward pagination never sees one —
    // we only ever hand out integer cursors).
    let start = after
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .map(|idx| idx + 1)
        .unwrap_or(0)
        .min(rows.len());
    let end = (start + limit).min(rows.len());

    let has_previous_page = start > 0;
    let has_next_page = end < rows.len();
    let page: Vec<(usize, RequestExecutionRow)> = rows
        .into_iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .collect();
    (page, total, has_previous_page, has_next_page)
}

// ---------------------------------------------------------------------------
// Where-filter evaluation
// ---------------------------------------------------------------------------

/// Strip a `gid://conduit/<Type>/` prefix so injected global ids and raw ids
/// both match the repo's raw integer-string ids. `rsplit` always yields at
/// least one segment, so the fallback arm is unreachable for non-empty input.
fn raw_id(id: &ID) -> &str {
    id.as_str().rsplit('/').next().unwrap_or(id.as_str())
}

/// eq/NEQ/In/NotIn over an ID-typed column value.
fn matches_id(
    value: &str,
    eq: Option<&ID>,
    neq: Option<&ID>,
    in_list: Option<&[ID]>,
    not_in: Option<&[ID]>,
) -> bool {
    if let Some(v) = eq
        && raw_id(v) != value
    {
        return false;
    }
    if let Some(v) = neq
        && raw_id(v) == value
    {
        return false;
    }
    if let Some(list) = in_list
        && !list.iter().any(|v| raw_id(v) == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|v| raw_id(v) == value)
    {
        return false;
    }
    true
}

/// eq/NEQ/In/NotIn over a string column value.
fn matches_str(
    value: &str,
    eq: Option<&str>,
    neq: Option<&str>,
    in_list: Option<&[String]>,
    not_in: Option<&[String]>,
) -> bool {
    if let Some(v) = eq
        && value != v
    {
        return false;
    }
    if let Some(v) = neq
        && value == v
    {
        return false;
    }
    if let Some(list) = in_list
        && !list.iter().any(|v| v == value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.iter().any(|v| v == value)
    {
        return false;
    }
    true
}

/// Contains/HasPrefix/HasSuffix over a string column value.
fn matches_substr(
    value: &str,
    contains: Option<&str>,
    has_prefix: Option<&str>,
    has_suffix: Option<&str>,
) -> bool {
    if let Some(v) = contains
        && !value.contains(v)
    {
        return false;
    }
    if let Some(v) = has_prefix
        && !value.starts_with(v)
    {
        return false;
    }
    if let Some(v) = has_suffix
        && !value.ends_with(v)
    {
        return false;
    }
    true
}

/// eq/NEQ/In/NotIn over an i64 column value.
fn matches_i64(
    value: i64,
    eq: Option<i64>,
    neq: Option<i64>,
    in_list: Option<&[i64]>,
    not_in: Option<&[i64]>,
) -> bool {
    if let Some(v) = eq
        && value != v
    {
        return false;
    }
    if let Some(v) = neq
        && value == v
    {
        return false;
    }
    if let Some(list) = in_list
        && !list.contains(&value)
    {
        return false;
    }
    if let Some(list) = not_in
        && list.contains(&value)
    {
        return false;
    }
    true
}

/// GT/GTE/LT/LTE over an i64 column value.
fn matches_i64_range(
    value: i64,
    gt: Option<i64>,
    gte: Option<i64>,
    lt: Option<i64>,
    lte: Option<i64>,
) -> bool {
    if let Some(v) = gt
        && value <= v
    {
        return false;
    }
    if let Some(v) = gte
        && value < v
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v
    {
        return false;
    }
    if let Some(v) = lte
        && value > v
    {
        return false;
    }
    true
}

/// eq/NEQ/GT/GTE/LT/LTE over a timestamp column value.
fn matches_time(
    value: DateTime<Utc>,
    eq: Option<&TimeScalar>,
    neq: Option<&TimeScalar>,
    gt: Option<&TimeScalar>,
    gte: Option<&TimeScalar>,
    lt: Option<&TimeScalar>,
    lte: Option<&TimeScalar>,
) -> bool {
    if let Some(v) = eq
        && value != v.0
    {
        return false;
    }
    if let Some(v) = neq
        && value == v.0
    {
        return false;
    }
    if let Some(v) = gt
        && value <= v.0
    {
        return false;
    }
    if let Some(v) = gte
        && value < v.0
    {
        return false;
    }
    if let Some(v) = lt
        && value >= v.0
    {
        return false;
    }
    if let Some(v) = lte
        && value > v.0
    {
        return false;
    }
    true
}

/// IsNil/NotNil over a nullable column. Ent applies each flag only when set to
/// `true` (the generated WhereInput appends the IsNull/NotNull predicate for a
/// truthy flag; `false` is the zero value and is dropped by `omitempty`).
fn matches_nil_flags<T>(value: &Option<T>, is_nil: Option<bool>, not_nil: Option<bool>) -> bool {
    if is_nil == Some(true) && value.is_some() {
        return false;
    }
    if not_nil == Some(true) && value.is_none() {
        return false;
    }
    true
}

/// Evaluate the implemented subset of `RequestExecutionWhereInput` against a
/// row. All top-level predicate groups AND together; `or` members OR among
/// themselves; `not` negates its nested input — matching ent's generated
/// `requestexecution.And(..., Or(...), Not(...))` composition.
///
/// SQL NULL semantics: value predicates (eq/NEQ/In/NotIn/Contains/...) on a
/// nullable column never match a NULL row — only IsNil does.
fn row_matches(filter: &RequestExecutionWhereInput, row: &RequestExecutionRow) -> bool {
    // --- combinators ---
    if let Some(not) = &filter.not
        && row_matches(not, row)
    {
        return false;
    }
    if let Some(and) = &filter.and
        && !and.iter().all(|child| row_matches(child, row))
    {
        return false;
    }
    if let Some(or) = &filter.or
        && !or.iter().any(|child| row_matches(child, row))
    {
        return false;
    }

    // --- id ---
    if !matches_id(
        &row.id,
        filter.id.as_ref(),
        filter.id_neq.as_ref(),
        filter.id_in.as_deref(),
        filter.id_not_in.as_deref(),
    ) {
        return false;
    }

    // --- created_at / updated_at ---
    if !matches_time(
        row.created_at,
        filter.created_at.as_ref(),
        filter.created_at_neq.as_ref(),
        filter.created_at_gt.as_ref(),
        filter.created_at_gte.as_ref(),
        filter.created_at_lt.as_ref(),
        filter.created_at_lte.as_ref(),
    ) {
        return false;
    }
    if !matches_time(
        row.updated_at,
        filter.updated_at.as_ref(),
        filter.updated_at_neq.as_ref(),
        filter.updated_at_gt.as_ref(),
        filter.updated_at_gte.as_ref(),
        filter.updated_at_lt.as_ref(),
        filter.updated_at_lte.as_ref(),
    ) {
        return false;
    }

    // --- project_id (Int predicates; row carries an integer string) ---
    let project_num: i64 = row.project_id.parse().unwrap_or(-1);
    if !matches_i64(
        project_num,
        filter.project_id,
        filter.project_id_neq,
        filter.project_id_in.as_deref(),
        filter.project_id_not_in.as_deref(),
    ) || !matches_i64_range(
        project_num,
        filter.project_id_gt,
        filter.project_id_gte,
        filter.project_id_lt,
        filter.project_id_lte,
    ) {
        return false;
    }

    // --- request_id (ID; this is the predicate the `Request.executions`
    //     edge resolver injects) ---
    if !matches_id(
        &row.request_id,
        filter.request_id.as_ref(),
        filter.request_id_neq.as_ref(),
        filter.request_id_in.as_deref(),
        filter.request_id_not_in.as_deref(),
    ) {
        return false;
    }

    // --- channel_id (nullable ID + nil flags) ---
    if !matches_nil_flags(
        &row.channel_id,
        filter.channel_id_is_nil,
        filter.channel_id_not_nil,
    ) {
        return false;
    }
    let has_channel_value_pred = filter.channel_id.is_some()
        || filter.channel_id_neq.is_some()
        || filter.channel_id_in.is_some()
        || filter.channel_id_not_in.is_some();
    if has_channel_value_pred {
        match &row.channel_id {
            // NULL never satisfies a value predicate (SQL semantics).
            None => return false,
            Some(value) => {
                if !matches_id(
                    value,
                    filter.channel_id.as_ref(),
                    filter.channel_id_neq.as_ref(),
                    filter.channel_id_in.as_deref(),
                    filter.channel_id_not_in.as_deref(),
                ) {
                    return false;
                }
            }
        }
    }

    // --- data_storage_id (nullable ID + nil flags) ---
    if !matches_nil_flags(
        &row.data_storage_id,
        filter.data_storage_id_is_nil,
        filter.data_storage_id_not_nil,
    ) {
        return false;
    }
    let has_storage_value_pred = filter.data_storage_id.is_some()
        || filter.data_storage_id_neq.is_some()
        || filter.data_storage_id_in.is_some()
        || filter.data_storage_id_not_in.is_some();
    if has_storage_value_pred {
        match &row.data_storage_id {
            None => return false,
            Some(value) => {
                if !matches_id(
                    value,
                    filter.data_storage_id.as_ref(),
                    filter.data_storage_id_neq.as_ref(),
                    filter.data_storage_id_in.as_deref(),
                    filter.data_storage_id_not_in.as_deref(),
                ) {
                    return false;
                }
            }
        }
    }

    // --- external_id (nullable String: eq/Contains + nil flags) ---
    if !matches_nil_flags(
        &row.external_id,
        filter.external_id_is_nil,
        filter.external_id_not_nil,
    ) {
        return false;
    }
    if filter.external_id.is_some() || filter.external_id_contains.is_some() {
        match &row.external_id {
            None => return false,
            Some(value) => {
                if !matches_str(value, filter.external_id.as_deref(), None, None, None)
                    || !matches_substr(value, filter.external_id_contains.as_deref(), None, None)
                {
                    return false;
                }
            }
        }
    }

    // --- model_id / format (String families) ---
    if !matches_str(
        &row.model_id,
        filter.model_id.as_deref(),
        filter.model_id_neq.as_deref(),
        filter.model_id_in.as_deref(),
        filter.model_id_not_in.as_deref(),
    ) || !matches_substr(
        &row.model_id,
        filter.model_id_contains.as_deref(),
        filter.model_id_has_prefix.as_deref(),
        filter.model_id_has_suffix.as_deref(),
    ) {
        return false;
    }
    if !matches_str(
        &row.format,
        filter.format.as_deref(),
        filter.format_neq.as_deref(),
        filter.format_in.as_deref(),
        filter.format_not_in.as_deref(),
    ) || !matches_substr(
        &row.format,
        filter.format_contains.as_deref(),
        filter.format_has_prefix.as_deref(),
        filter.format_has_suffix.as_deref(),
    ) {
        return false;
    }

    // --- error_message (nullable String: eq/Contains + nil flags) ---
    if !matches_nil_flags(
        &row.error_message,
        filter.error_message_is_nil,
        filter.error_message_not_nil,
    ) {
        return false;
    }
    if filter.error_message.is_some() || filter.error_message_contains.is_some() {
        match &row.error_message {
            None => return false,
            Some(value) => {
                if !matches_str(value, filter.error_message.as_deref(), None, None, None)
                    || !matches_substr(value, filter.error_message_contains.as_deref(), None, None)
                {
                    return false;
                }
            }
        }
    }

    // --- response_status_code (nullable Int: full numeric family) ---
    if !matches_nil_flags(
        &row.response_status_code,
        filter.response_status_code_is_nil,
        filter.response_status_code_not_nil,
    ) {
        return false;
    }
    let has_code_value_pred = filter.response_status_code.is_some()
        || filter.response_status_code_neq.is_some()
        || filter.response_status_code_in.is_some()
        || filter.response_status_code_not_in.is_some()
        || filter.response_status_code_gt.is_some()
        || filter.response_status_code_gte.is_some()
        || filter.response_status_code_lt.is_some()
        || filter.response_status_code_lte.is_some();
    if has_code_value_pred {
        match row.response_status_code {
            None => return false,
            Some(value) => {
                if !matches_i64(
                    value,
                    filter.response_status_code,
                    filter.response_status_code_neq,
                    filter.response_status_code_in.as_deref(),
                    filter.response_status_code_not_in.as_deref(),
                ) || !matches_i64_range(
                    value,
                    filter.response_status_code_gt,
                    filter.response_status_code_gte,
                    filter.response_status_code_lt,
                    filter.response_status_code_lte,
                ) {
                    return false;
                }
            }
        }
    }

    // --- status (enum: eq/NEQ/In/NotIn — full family) ---
    if let Some(v) = filter.status
        && row.status != status_to_str(v)
    {
        return false;
    }
    if let Some(v) = filter.status_neq
        && row.status == status_to_str(v)
    {
        return false;
    }
    if let Some(list) = &filter.status_in
        && !list.iter().any(|v| row.status == status_to_str(*v))
    {
        return false;
    }
    if let Some(list) = &filter.status_not_in
        && list.iter().any(|v| row.status == status_to_str(*v))
    {
        return false;
    }

    // --- stream / pass_through_applied (bool: eq/NEQ — full families) ---
    if let Some(v) = filter.stream
        && row.stream != v
    {
        return false;
    }
    if let Some(v) = filter.stream_neq
        && row.stream == v
    {
        return false;
    }
    if let Some(v) = filter.pass_through_applied
        && row.pass_through_applied != v
    {
        return false;
    }
    if let Some(v) = filter.pass_through_applied_neq
        && row.pass_through_applied == v
    {
        return false;
    }

    // Everything else (metrics families, requestURL family, string ordering
    // and fold variants, createdAtIn/NotIn, ...) is DEFERRED — see the module
    // header. Deferred predicates are ignored rather than erroring, matching
    // the deferred-predicate policy in `wiring_requests.rs`.
    true
}

// ---------------------------------------------------------------------------
// Tests — mirror the in-crate mock golden cases
// (`request_executions_returns_connection_shape`,
// `request_executions_applies_status_where_filter`) plus the edge-injected
// requestID predicate semantics, against the real adapter + repository.
// ---------------------------------------------------------------------------

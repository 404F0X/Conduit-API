//! Usage-log-only [`RequestRecorder`] for the production binary.
//!
//! # Why this exists (the gap it closes)
//!
//! The binary previously wired [`NoopRequestRecorder`], so **no usage-log
//! (billing / token-accounting) rows were ever written** — a silent data loss
//! for the dashboard's usage + cost pages.
//!
//! The request / execution *rows* are already persisted by the pipeline's
//! `PersistRequestMiddleware` / `PersistRequestExecutionMiddleware`
//! (pending → processing → completed/failed, with body + metrics). The ONLY
//! thing those middlewares do NOT write is the usage-log row (Go's
//! `persistRequest.OnOutboundLlmResponse` →
//! `UsageLogService.CreateUsageLogFromRequest`).
//!
//! Wiring the full `ProductionRequestRecorder` instead would **double-write**
//! the request/execution terminal state (it also calls
//! `update_request_completed` / `update_request_execution_completed`) and would
//! need its execution id (`{id}-attempt-{n}`) to match the middleware's
//! (`{id}-exec-{ts}`) — it does not. So this recorder deliberately does ONE
//! thing: build the usage-log row from the response's structured
//! [`conduit_llm::Usage`] and insert it, mirroring the Go usage-log write while
//! staying conflict-free with the persist middlewares.
//!
//! # Go contract fidelity
//!
//! * Writes only on success when token usage is non-zero, or when the resolved
//!   channel price contains a per-request `flat_fee`. The latter covers image,
//!   audio, and video providers that return no token usage at all.
//! * A usage-log write failure is logged and swallowed — it MUST NOT fail the
//!   request (Go: `log.Warn` + continue). `record_success` therefore always
//!   returns `Ok`.
//! * `record_failure` is a no-op: the failed request/execution rows are the
//!   persist middlewares' responsibility, and a failed request has no usage to
//!   bill.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use conduit_cache::Cache;
use conduit_core::ConduitError;
use conduit_core::objects::money::AccountingSettings;
use conduit_core::objects::pricing::{ModelPrice, PRICING_MODE_FLAT_FEE};
use conduit_db::repo::channel_model_price_repo::ChannelModelPriceRepo;
use conduit_db::repo::usage_repo::{CreateUsageLogInput, UsageRepo};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_llm::HttpResponse;
use conduit_orchestrator::orchestrator::{OrchestratorContext, RequestRecorder};
use conduit_pipeline::pipeline::AttemptRecord as PipelineAttempt;
use conduit_services::usage_service::{
    CreateUsageLogParams, ResolvedModelPrice, UsageLog, UsageLogSource,
    create_usage_log_from_structured_usage,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::warn;

/// Default request format stamped on usage-log rows when the pipeline does not
/// surface a more specific one. Mirrors Go's
/// `field.String("format").Default("openai/chat_completions")`.
const DEFAULT_FORMAT: &str = "openai/chat_completions";

/// A [`RequestRecorder`] that persists ONLY the usage-log row, backed by a
/// [`UsageRepo`]. See the module docs for why it is scoped this narrowly.
///
/// When a [`ChannelModelPriceRepo`] is wired via [`UsageLogRecorder::with_price_repo`],
/// the recorder also resolves the per-channel model price at persist time and
/// computes `total_cost` (mirroring Go `usage_log.go:52-53`
/// `ch.cachedModelPrices[modelID]` → `ComputeUsageCost`). Without it, the
/// recorder falls back to the S11 no-cost path (`total_cost = 0`) so token
/// accounting still works.
pub struct UsageLogRecorder {
    usage_repo: Arc<dyn UsageRepo>,
    sticky_channel_cache: Option<Arc<dyn Cache>>,
    route_affinity: Option<Arc<crate::route_affinity::RouteAffinityRuntime>>,
    /// Optional per-channel model-price source. `None` = no-cost fallback
    /// (token counts recorded, `total_cost` left at 0).
    price_repo: Option<Arc<dyn ChannelModelPriceRepo>>,
    charge_settler: Option<Arc<dyn crate::usage_charge_settler::UsageChargeSettler>>,
    /// Live SSE bypasses the non-stream response middleware that normally
    /// finalizes request/execution rows. This pool is used only by the stream
    /// recorder callbacks to close those already-created rows and save chunks.
    stream_persistence: Option<StreamPersistence>,
    /// Active request-scoped lease ids grouped by API key. Tracking the lease
    /// rather than only a counter makes release idempotent: a timeout cleanup
    /// racing a normal recorder callback cannot decrement a later request's
    /// slot.
    api_key_concurrency: Mutex<HashMap<i64, HashSet<String>>>,
}

enum StreamPersistence {
    Postgres(PgPool),
}

struct ResolvedChannelPrice {
    price: ModelPrice,
    reference_id: String,
    currency_code: String,
}

struct AccountingConversionAudit {
    source_currency: String,
    source_total: Decimal,
    source_subtotals: Vec<Decimal>,
    accounting_currency: Option<String>,
    quote_per_accounting_unit: Option<Decimal>,
    accounting_total: Option<Decimal>,
    accounting_subtotals: Vec<Option<Decimal>>,
    accounting_settings_version: Option<u64>,
    status: &'static str,
    error: Option<String>,
}

impl AccountingConversionAudit {
    fn attach_to_cost_items(&self, cost_items: &mut Value) {
        let Some(items) = cost_items.as_array_mut() else {
            return;
        };
        for (index, item) in items.iter_mut().enumerate() {
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            let source_subtotal = self.source_subtotals.get(index).copied();
            let accounting_subtotal = self.accounting_subtotals.get(index).copied().flatten();
            item.insert(
                "accountingConversion".into(),
                json!({
                    "sourceCurrency": self.source_currency,
                    "sourceSubtotal": source_subtotal.map(decimal_string),
                    "sourceTotal": decimal_string(self.source_total),
                    "quotePerAccountingUnit": self.quote_per_accounting_unit.map(decimal_string),
                    "accountingCurrency": self.accounting_currency,
                    "accountingSubtotal": accounting_subtotal.map(decimal_string),
                    "accountingTotal": self.accounting_total.map(decimal_string),
                    "accountingSettingsVersion": self.accounting_settings_version,
                    "status": self.status,
                    "error": self.error,
                }),
            );
        }
    }
}

fn decimal_string(value: Decimal) -> String {
    value.normalize().to_string()
}

/// Convert a channel import cost into the system accounting currency while
/// retaining enough source data to audit the exact FX decision later.
/// Conversion failure deliberately clears `total_cost`: treating an unknown
/// foreign amount as though it were already in the accounting currency would
/// corrupt every cost/profit aggregate.
fn convert_import_cost_to_accounting(
    mut log: UsageLog,
    source_currency: &str,
    settings: Result<AccountingSettings, String>,
) -> (UsageLog, AccountingConversionAudit) {
    let source_currency = source_currency.trim().to_ascii_uppercase();
    let source_total = log.total_cost.unwrap_or(Decimal::ZERO);
    let source_subtotals = log
        .cost_items
        .iter()
        .map(|item| item.detail.subtotal)
        .collect::<Vec<_>>();

    let failed = |log: UsageLog,
                  accounting_currency: Option<String>,
                  accounting_settings_version: Option<u64>,
                  error: String| {
        let mut log = log;
        log.total_cost = None;
        let audit = AccountingConversionAudit {
            source_currency: source_currency.clone(),
            source_total,
            source_subtotals: source_subtotals.clone(),
            accounting_currency,
            quote_per_accounting_unit: None,
            accounting_total: None,
            accounting_subtotals: vec![None; source_subtotals.len()],
            accounting_settings_version,
            status: "failed",
            error: Some(error),
        };
        (log, audit)
    };

    let settings = match settings {
        Ok(settings) => settings,
        Err(error) => return failed(log, None, None, error),
    };
    let accounting_currency = settings.accounting_currency.clone();
    let settings_version = settings.version;
    let quote = match settings.accounting_to_real(Decimal::ONE, &source_currency) {
        Ok(quote) => quote,
        Err(error) => {
            return failed(
                log,
                Some(accounting_currency),
                Some(settings_version),
                error,
            );
        }
    };

    let accounting_total = source_total / quote;
    for item in &mut log.cost_items {
        item.detail.subtotal /= quote;
        for tier in &mut item.detail.tier_breakdown {
            tier.subtotal /= quote;
        }
    }
    log.total_cost = Some(accounting_total);
    let accounting_subtotals = log
        .cost_items
        .iter()
        .map(|item| Some(item.detail.subtotal))
        .collect();
    let audit = AccountingConversionAudit {
        source_currency,
        source_total,
        source_subtotals,
        accounting_currency: Some(accounting_currency),
        quote_per_accounting_unit: Some(quote),
        accounting_total: Some(accounting_total),
        accounting_subtotals,
        accounting_settings_version: Some(settings_version),
        status: "converted",
        error: None,
    };
    (log, audit)
}

fn stream_chunks_value(chunks: &[conduit_llm::StreamEvent]) -> Value {
    serde_json::to_value(chunks).unwrap_or_else(|_| Value::Array(Vec::new()))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_credential_fingerprint(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(is_sha256_hex)
}

fn response_id(response: &HttpResponse) -> Option<String> {
    response
        .json_body
        .clone()
        .or_else(|| {
            response
                .body
                .as_deref()
                .and_then(|body| serde_json::from_slice(body).ok())
        })?
        .get("id")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn stream_metric_ms(
    ctx: &OrchestratorContext,
    response: Option<&HttpResponse>,
    response_key: &str,
    context_key: &str,
) -> Option<i64> {
    response
        .and_then(|response| response.metadata.get(response_key))
        .and_then(Value::as_i64)
        .or_else(|| {
            ctx.metadata
                .get(context_key)
                .and_then(|value| value.parse().ok())
        })
}

impl UsageLogRecorder {
    /// Wire the recorder with a real usage repo. Cost stays at
    /// the no-cost fallback until [`UsageLogRecorder::with_price_repo`] adds a
    /// price source.
    pub fn new(usage_repo: Arc<dyn UsageRepo>) -> Self {
        Self {
            usage_repo,
            sticky_channel_cache: None,
            route_affinity: None,
            price_repo: None,
            charge_settler: None,
            stream_persistence: None,
            api_key_concurrency: Mutex::new(HashMap::new()),
        }
    }

    /// Attach a per-channel model-price source so `record_success` resolves the
    /// price and computes `total_cost` (Go `ComputeUsageCost` parity). Chainable
    /// after [`UsageLogRecorder::new`].
    pub fn with_price_repo(mut self, price_repo: Arc<dyn ChannelModelPriceRepo>) -> Self {
        self.price_repo = Some(price_repo);
        self
    }

    pub fn with_sticky_channel_cache(mut self, cache: Arc<dyn Cache>) -> Self {
        self.sticky_channel_cache = Some(cache);
        self
    }

    pub fn with_route_affinity_runtime(
        mut self,
        runtime: Option<Arc<crate::route_affinity::RouteAffinityRuntime>>,
    ) -> Self {
        self.route_affinity = runtime;
        self
    }

    pub fn with_charge_settler(
        mut self,
        charge_settler: Arc<dyn crate::usage_charge_settler::UsageChargeSettler>,
    ) -> Self {
        self.charge_settler = Some(charge_settler);
        self
    }
    pub fn with_postgres_stream_persistence(mut self, pool: PgPool) -> Self {
        self.stream_persistence = Some(StreamPersistence::Postgres(pool));
        self
    }

    async fn finish_stream_request(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        response: &HttpResponse,
    ) {
        let Some(persistence) = &self.stream_persistence else {
            return;
        };
        let latency_ms = stream_metric_ms(ctx, Some(response), "latency_ms", "perf_latency_ms");
        let first_token_latency_ms = stream_metric_ms(
            ctx,
            Some(response),
            "first_token_latency_ms",
            "first_token_latency_ms",
        );
        let channel_id = ctx
            .metadata
            .get("channel_id")
            .and_then(|value| value.parse::<i64>().ok());
        let result = match persistence {
            StreamPersistence::Postgres(pool) => {
                let request_id = request_id.parse::<i64>();
                let project_id = project_id.parse::<i64>();
                match (request_id, project_id) {
                    (Ok(request_id), Ok(project_id)) => sqlx::query(
                        "UPDATE requests SET status='completed',response_body=COALESCE($1::jsonb,response_body), \
                         metrics_latency_ms=COALESCE($2,metrics_latency_ms), \
                         metrics_first_token_latency_ms=COALESCE($3,metrics_first_token_latency_ms), \
                         channel_id=COALESCE($4,channel_id), \
                         updated_at=CURRENT_TIMESTAMP WHERE id=$5 AND project_id=$6 \
                         AND status IN ('pending','processing')",
                    )
                    .bind(response.json_body.clone())
                    .bind(latency_ms)
                    .bind(first_token_latency_ms)
                    .bind(channel_id)
                    .bind(request_id)
                    .bind(project_id)
                    .execute(pool)
                    .await
                    .map(|_| ()),
                    _ => {
                        warn!("stream recorder: invalid PostgreSQL request/project id");
                        return;
                    }
                }
            }
        };
        if let Err(error) = result {
            warn!(%error, request_id, "stream recorder: failed to complete request row");
        }
    }

    async fn remember_successful_channel(&self, ctx: &OrchestratorContext, channel_id: &str) {
        const STICKY_CHANNEL_TTL: std::time::Duration = std::time::Duration::from_secs(60);

        let Some(cache) = &self.sticky_channel_cache else {
            return;
        };
        let Some(trace_id) = ctx
            .metadata
            .get("trace_id")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
        else {
            return;
        };
        let Ok(channel_id) = channel_id.parse::<i64>() else {
            return;
        };
        if channel_id <= 0 {
            return;
        }

        let cache_key = crate::route_affinity::sticky_channel_cache_key(&trace_id.to_string());
        let Some(value) = crate::route_affinity::sticky_channel_cache_value(
            Some(channel_id.to_string()),
            chrono::Utc::now(),
            STICKY_CHANNEL_TTL,
        ) else {
            warn!(trace_id, "sticky channel cache TTL is outside chrono range");
            return;
        };
        if let Err(error) = cache.set(&cache_key, value, Some(STICKY_CHANNEL_TTL)).await {
            warn!(
                %error,
                trace_id,
                channel_id,
                "usage-log recorder: failed to cache successful trace channel"
            );
        }
    }

    async fn remember_explicit_route_affinity(
        &self,
        ctx: &OrchestratorContext,
        project_id: &str,
        channel_id: &str,
        response: &HttpResponse,
    ) {
        use conduit_orchestrator::orchestrator::{
            ROUTE_AFFINITY_API_FORMAT_METADATA, ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA,
            ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA, ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
        };

        let Some(runtime) = self.route_affinity.as_ref() else {
            return;
        };
        let Some(public_model_id) = ctx.metadata.get(ROUTE_AFFINITY_PUBLIC_MODEL_METADATA) else {
            return;
        };
        let Some(api_format) = ctx.metadata.get(ROUTE_AFFINITY_API_FORMAT_METADATA) else {
            return;
        };
        let Some(upstream_model_id) = ctx
            .metadata
            .get("actual_model")
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        let Some(upstream_api_format) =
            ctx.metadata.get("format").filter(|value| !value.is_empty())
        else {
            return;
        };
        let credential_identity = ctx
            .metadata
            .get("credential_identity")
            .filter(|value| is_credential_fingerprint(value))
            .cloned();
        if ctx.metadata.contains_key("credential_identity") && credential_identity.is_none() {
            warn!("usage-log recorder: ignored invalid credential identity for route affinity");
        }
        let mut hashes = Vec::new();
        if let Some(hash) = ctx
            .metadata
            .get(ROUTE_AFFINITY_PREVIOUS_RESPONSE_HASH_METADATA)
            .filter(|hash| is_sha256_hex(hash))
        {
            hashes.push((conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID, hash.clone()));
        }
        if let Some(hash) = ctx
            .metadata
            .get(ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA)
            .filter(|hash| is_sha256_hex(hash))
        {
            hashes.push((conduit_db::KEY_CLASS_PROMPT_CACHE_KEY, hash.clone()));
        }
        if matches!(
            api_format.as_str(),
            "openai/responses" | "openai/responses_compact"
        ) && let Some(response_id) = response_id(response)
        {
            let hash = crate::route_affinity::hash_explicit_affinity_value(&response_id);
            if !hashes.iter().any(|(key_class, existing)| {
                *key_class == conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID && existing == &hash
            }) {
                hashes.push((conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID, hash));
            }
        }

        let now = chrono::Utc::now();
        for (key_class, key_hash) in hashes {
            let Ok(ttl) = chrono::Duration::from_std(runtime.ttl_for_key_class(key_class)) else {
                warn!(key_class, "route affinity TTL is outside chrono range");
                continue;
            };
            let input = conduit_db::UpsertRouteAffinityInput {
                key: conduit_db::RouteAffinityKey {
                    project_id: project_id.to_string(),
                    key_class: key_class.to_string(),
                    key_hash,
                    public_model_id: public_model_id.clone(),
                    api_format: api_format.clone(),
                },
                channel_id: channel_id.to_string(),
                upstream_model_id: upstream_model_id.clone(),
                upstream_api_format: upstream_api_format.clone(),
                credential_identity: credential_identity.clone(),
                expires_at: now + ttl,
            };
            if let Err(error) = runtime.remember(input).await {
                warn!(
                    %error,
                    key_class,
                    "usage-log recorder: failed to persist route affinity"
                );
            }
        }
    }

    /// Resolve the channel's current [`ModelPrice`] for `model_id`, mirroring Go
    /// `ch.cachedModelPrices[modelID]` (`usage_log.go:52`): list the channel's
    /// live price head rows and match on `model_id`, deserializing the stored
    /// `price` JSON into a [`ModelPrice`]. The source currency travels with the
    /// price so the resulting procurement cost can be normalized before it is
    /// persisted. Any error / miss → `None` (no-cost fallback; never fails the
    /// write).
    async fn resolve_price(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        model_id: &str,
    ) -> Option<ResolvedChannelPrice> {
        let repo = self.price_repo.as_ref()?;
        let rows = match repo.list_prices_by_channel(ctx, channel_id).await {
            Ok(rows) => rows,
            Err(err) => {
                warn!(error = %err, channel_id, "usage-log recorder: price lookup failed (no-cost fallback)");
                return None;
            }
        };
        let row = rows.into_iter().find(|r| r.model_id == model_id)?;
        match serde_json::from_value::<ModelPrice>(row.price) {
            Ok(price) => Some(ResolvedChannelPrice {
                price,
                reference_id: row.reference_id,
                currency_code: row.currency_code,
            }),
            Err(err) => {
                warn!(error = %err, channel_id, model_id, "usage-log recorder: price JSON parse failed (no-cost fallback)");
                None
            }
        }
    }

    /// Resolve the actual model id at persist time. The recorder runs detached
    /// from the pipeline context, so the model is best-effort: the orchestrator
    /// context surfaces `actual_model` / `model` / `original_model` when a
    /// middleware stamped it, otherwise we fall back to an empty string (Go
    /// stores the actual model id; precise attribution lands when the pipeline
    /// stamps it onto the orchestrator context).
    fn resolve_model(ctx: &OrchestratorContext) -> String {
        for key in ["actual_model", "model", "original_model"] {
            if let Some(model) = ctx.metadata.get(key)
                && !model.is_empty()
            {
                return model.clone();
            }
        }
        String::new()
    }

    fn has_request_flat_fee(price: &ModelPrice) -> bool {
        price.items.iter().any(|item| {
            item.pricing.mode == PRICING_MODE_FLAT_FEE
                && item.pricing.flat_fee.is_some_and(|fee| !fee.is_zero())
        })
    }

    async fn accounting_settings(&self) -> Result<AccountingSettings, String> {
        let Some(StreamPersistence::Postgres(pool)) = self.stream_persistence.as_ref() else {
            return Err("accounting settings PostgreSQL pool is not configured".into());
        };
        crate::usage_charge_settler_postgres::load_accounting_settings(pool).await
    }
}

#[async_trait]
impl RequestRecorder for UsageLogRecorder {
    fn acquire_api_key_slot(
        &self,
        ctx: &mut OrchestratorContext,
        api_key_id: i64,
        limit: u32,
    ) -> Result<(), ConduitError> {
        let mut counts = self
            .api_key_concurrency
            .lock()
            .map_err(|_| ConduitError::internal("API key concurrency lock poisoned"))?;
        let leases = counts.entry(api_key_id).or_default();
        let current = leases.len();
        if current >= limit as usize {
            return Err(ConduitError::rate_limited(format!(
                "API key concurrency limit exceeded ({current}/{limit})"
            )));
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        leases.insert(lease_id.clone());
        ctx.metadata.insert(
            "api_key_concurrency_slot".to_string(),
            api_key_id.to_string(),
        );
        ctx.metadata
            .insert("api_key_concurrency_lease".to_string(), lease_id);
        Ok(())
    }

    fn release_api_key_slot(&self, ctx: &OrchestratorContext) {
        let (Some(api_key_id), Some(lease_id)) = (
            ctx.metadata
                .get("api_key_concurrency_slot")
                .and_then(|value| value.parse::<i64>().ok()),
            ctx.metadata.get("api_key_concurrency_lease"),
        ) else {
            return;
        };
        let Ok(mut counts) = self.api_key_concurrency.lock() else {
            return;
        };
        let remove_key = counts.get_mut(&api_key_id).is_some_and(|leases| {
            leases.remove(lease_id);
            leases.is_empty()
        });
        if remove_key {
            counts.remove(&api_key_id);
        }
    }

    fn abandon_request(&self, ctx: &OrchestratorContext, reason: &'static str) {
        // In-memory admission is released synchronously from Drop so a second
        // request can proceed immediately after timeout/cancellation.
        self.release_api_key_slot(ctx);

        // Durable wallet cleanup is async. PostgreSQL release is idempotent,
        // and reservations also carry a 15-minute expiry consumed by the
        // reconciler, so a runtime shutting down before this task runs cannot
        // leave funds reserved indefinitely.
        let (Some(settler), Some(reservation_key)) = (
            self.charge_settler.as_ref().cloned(),
            ctx.metadata
                .get("billing_reservation_key")
                .or_else(|| {
                    ctx.metadata.get(
                        conduit_orchestrator::orchestrator::BILLING_ADMISSION_REQUEST_KEY_METADATA,
                    )
                })
                .cloned(),
        ) else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                reservation_key = %reservation_key,
                "wallet cancellation cleanup deferred to reservation expiry because no Tokio runtime is active"
            );
            return;
        };
        handle.spawn(async move {
            if let Err(error) = settler.release_request(&reservation_key, reason).await {
                warn!(
                    %error,
                    reservation_key = %reservation_key,
                    "wallet cancellation cleanup failed; reservation expiry will retry"
                );
            }
        });
    }

    async fn reserve_request(
        &self,
        ctx: &mut OrchestratorContext,
        input: &conduit_orchestrator::orchestrator::BillingAdmissionInput,
    ) -> Result<(), ConduitError> {
        let Some(settler) = &self.charge_settler else {
            return Ok(());
        };
        match settler.reserve_request(input).await {
            Ok(Some(key)) => {
                ctx.metadata
                    .insert("billing_reservation_key".to_string(), key);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => Err(ConduitError::quota_exhausted(error)),
        }
    }

    /// Build + insert the usage-log row from the successful response's
    /// structured usage. Always returns `Ok` (a usage-log write must never mask
    /// a successful request — Go `log.Warn` + continue).
    async fn record_success(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        attempt: &PipelineAttempt,
        response: &HttpResponse,
    ) -> Result<(), ConduitError> {
        self.release_api_key_slot(ctx);
        if matches!(
            attempt.mode,
            conduit_pipeline::pipeline::ExecutionMode::Stream
        ) {
            self.finish_stream_request(ctx, request_id, project_id, response)
                .await;
        }
        self.remember_successful_channel(ctx, &attempt.channel_id)
            .await;
        self.remember_explicit_route_affinity(ctx, project_id, &attempt.channel_id, response)
            .await;

        // Ids arrive as strings on the orchestrator contract; the Go Ent schema
        // types them as int. A non-numeric id would never occur in Go, so we
        // treat a parse failure as "drop the row + warn" (never fail the
        // request).
        let request_id_i64 = match request_id.parse::<i64>() {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    request_id,
                    "usage-log recorder: non-numeric request id; skipping usage log"
                );
                return Ok(());
            }
        };
        let project_id_i64 = match project_id.parse::<i64>() {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    project_id,
                    "usage-log recorder: non-numeric project id; skipping usage log"
                );
                return Ok(());
            }
        };
        let channel_id = attempt.channel_id.parse::<i64>().ok();
        // Prefer the model the upstream actually echoed in the response body
        // (Go's `ActualModelID`); fall back to whatever a middleware stamped on
        // the orchestrator context. `HttpResponse` carries no typed `model`
        // field — the provider's model is in the JSON body's top-level "model"
        // key — because the pipeline does not currently stamp `actual_model`
        // onto the orchestrator context.
        let model = response
            .json_body
            .as_ref()
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Self::resolve_model(ctx));
        self.persist_route_explanation(
            ctx,
            request_id,
            project_id,
            Some(attempt.channel_id.as_str()),
            Some(model.as_str()),
            None,
        )
        .await;

        // Detached, system-principal context: the recorder writes regardless of
        // the original caller's scopes (Go's detached persist inherits no auth).
        let svc_ctx = RequestContext::new(PolicyContext::new(Principal::system()));

        // Resolve the per-channel model price (Go `usage_log.go:52-53`
        // `ch.cachedModelPrices[modelID]` → `ComputeUsageCost`). A price hit
        // computes `total_cost`; a miss (no repo, disabled channel, unpriced
        // model, or parse error) leaves the S11 no-cost fallback so token
        // accounting still records.
        let resolved_price = match channel_id {
            Some(cid) if !model.is_empty() => self.resolve_price(&svc_ctx, cid, &model).await,
            _ => None,
        };

        // Token-priced responses need non-zero structured usage. Per-request
        // prices are different: several OpenAI-compatible media endpoints do
        // not return usage, but a successful request still incurs the flat
        // upstream fee. Feed an all-zero Usage into the existing cost engine;
        // `flat_fee` deliberately ignores token quantity. If neither condition
        // applies, retain the old behavior and avoid empty accounting rows.
        let default_usage = conduit_llm::Usage::default();
        let usage = match response.usage.as_ref() {
            Some(usage) if !usage.is_zero() => usage,
            _ if resolved_price
                .as_ref()
                .is_some_and(|resolved| Self::has_request_flat_fee(&resolved.price)) =>
            {
                &default_usage
            }
            _ => {
                self.release_reservation(ctx, "successful_unmetered_request")
                    .await;
                return Ok(());
            }
        };

        // Resolve the API key id from the orchestrator context metadata
        // (stamped by the orchestrator's identity-key copy). Without this the
        // usage_logs row's `api_key_id` is NULL, and the auth-time per-key
        // quota checks (hour/day/token/cost) that filter usage_logs by
        // `api_key_id` count zero rows and never fire (P-44). A present-but-
        // unparseable value is logged rather than silently dropped.
        let api_key_id = ctx.metadata.get("api_key_id").and_then(|raw| {
            match raw.parse::<i64>() {
                Ok(id) => Some(id),
                Err(err) => {
                    warn!(
                        raw = %raw,
                        %err,
                        "usage-log recorder: api_key_id in context is not a valid i64; recording NULL"
                    );
                    None
                }
            }
        });

        // Build the fully-populated row from STRUCTURED usage (S14 contract —
        // no raw-body parsing). `resolved_price` feeds `ComputeUsageCost`; when
        // `None`, the no-cost fallback applies (token data still recorded).
        let request_format = ctx
            .metadata
            .get("format")
            .map(String::as_str)
            .unwrap_or(DEFAULT_FORMAT);
        let params = CreateUsageLogParams::new(
            request_id_i64,
            project_id_i64,
            channel_id,
            &model,
            usage,
            UsageLogSource::Api,
            request_format,
            api_key_id,
        );
        let params = match resolved_price.as_ref() {
            Some(resolved) => params.with_resolved_price(ResolvedModelPrice {
                price: &resolved.price,
                reference_id: resolved.reference_id.as_str(),
            }),
            None => params,
        };
        let usage_log = create_usage_log_from_structured_usage(params);
        let (usage_log, conversion_audit) = match resolved_price.as_ref() {
            Some(resolved) => {
                let settings = self.accounting_settings().await;
                let (usage_log, audit) =
                    convert_import_cost_to_accounting(usage_log, &resolved.currency_code, settings);
                if audit.status == "failed" {
                    warn!(
                        source_currency = %audit.source_currency,
                        error = audit.error.as_deref().unwrap_or("unknown conversion error"),
                        "usage-log recorder: import cost FX conversion failed; total_cost left NULL"
                    );
                }
                (usage_log, Some(audit))
            }
            None => (usage_log, None),
        };
        let mut input = usage_log_to_create_input(usage_log);
        if let Some(audit) = conversion_audit {
            audit.attach_to_cost_items(&mut input.cost_items);
        }
        match self.usage_repo.insert_usage(&svc_ctx, input).await {
            Ok(created) => {
                if let Some(settler) = &self.charge_settler
                    && let Err(err) = settler
                        .settle_usage(
                            &created,
                            usage,
                            ctx.metadata
                                .get("billing_reservation_key")
                                .map(String::as_str),
                        )
                        .await
                {
                    // Usage remains authoritative even if customer charging
                    // fails. The missing unique charge event is observable and
                    // can be reconciled without billing the request twice.
                    warn!(error = %err, usage_log_id = %created.id, "usage charge settlement failed (non-fatal)");
                }
            }
            Err(err) => {
                // Non-fatal: mirror Go's `log.Warn` — a usage-log failure must not
                // mask a successful request.
                warn!(error = %err, "usage-log recorder: insert_usage failed (non-fatal)");
                self.release_reservation(ctx, "usage_log_persist_failed")
                    .await;
            }
        }
        Ok(())
    }

    /// No-op: the persist middlewares own the failed request/execution rows, and
    /// a failed request has no usage to bill.
    async fn record_failure(
        &self,
        _ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        error: &ConduitError,
    ) -> Result<(), ConduitError> {
        self.release_api_key_slot(_ctx);
        self.release_reservation(_ctx, &format!("request_failed:{error}"))
            .await;
        self.persist_route_explanation(
            _ctx,
            request_id,
            project_id,
            _ctx.metadata.get("channel_id").map(String::as_str),
            _ctx.metadata
                .get("actual_model")
                .or_else(|| _ctx.metadata.get("request_model"))
                .map(String::as_str),
            Some(&error.to_string()),
        )
        .await;
        let Some(persistence) = &self.stream_persistence else {
            return Ok(());
        };
        let result = match persistence {
            StreamPersistence::Postgres(pool) => {
                match (request_id.parse::<i64>(), project_id.parse::<i64>()) {
                    (Ok(request_id), Ok(project_id)) => sqlx::query(
                        "UPDATE requests SET status='failed',updated_at=CURRENT_TIMESTAMP \
                     WHERE id=$1 AND project_id=$2 AND status IN ('pending','processing')",
                    )
                    .bind(request_id)
                    .bind(project_id)
                    .execute(pool)
                    .await
                    .map(|_| ()),
                    _ => return Ok(()),
                }
            }
        };
        if let Err(error) = result {
            warn!(%error, request_id, "stream recorder: failed to mark request failed");
        }
        Ok(())
    }

    async fn record_stream_final(
        &self,
        ctx: &OrchestratorContext,
        _request_id: &str,
        project_id: &str,
        plan: &conduit_orchestrator::orchestrator::StreamFinalPlan,
        execution_id: Option<&str>,
        aggregated: Option<&HttpResponse>,
        chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        let (Some(persistence), Some(execution_id)) = (&self.stream_persistence, execution_id)
        else {
            return Ok(());
        };
        let status = if plan.is_completed() {
            "completed"
        } else if plan.is_canceled() {
            "canceled"
        } else {
            "failed"
        };
        let latency_ms = plan
            .is_completed()
            .then(|| stream_metric_ms(ctx, aggregated, "latency_ms", "perf_latency_ms"))
            .flatten();
        let first_token_latency_ms = plan
            .is_completed()
            .then(|| {
                stream_metric_ms(
                    ctx,
                    aggregated,
                    "first_token_latency_ms",
                    "first_token_latency_ms",
                )
            })
            .flatten();
        let chunks_json = if plan.write_chunks {
            Some(stream_chunks_value(chunks))
        } else {
            None
        };
        let result = match persistence {
            StreamPersistence::Postgres(pool) => match (execution_id.parse::<i64>(), project_id.parse::<i64>()) {
                (Ok(execution_id), Ok(project_id)) => sqlx::query(
                    "UPDATE request_executions SET status=$1,response_chunks=COALESCE($2::jsonb,response_chunks), \
                     error_message=COALESCE($3,error_message),metrics_latency_ms=COALESCE($4,metrics_latency_ms), \
                     metrics_first_token_latency_ms=COALESCE($5,metrics_first_token_latency_ms), \
                     updated_at=CURRENT_TIMESTAMP WHERE id=$6 AND project_id=$7",
                ).bind(status).bind(chunks_json).bind(plan.error_message.as_deref())
                    .bind(latency_ms).bind(first_token_latency_ms).bind(execution_id).bind(project_id)
                    .execute(pool).await.map(|_|()),
                _ => return Ok(()),
            },
        };
        if let Err(error) = result {
            warn!(%error, execution_id, "stream recorder: failed to finalize execution row");
        }
        Ok(())
    }

    async fn record_stream_request_chunks(
        &self,
        _ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        chunks: &[conduit_llm::StreamEvent],
    ) -> Result<(), ConduitError> {
        let Some(persistence) = &self.stream_persistence else {
            return Ok(());
        };
        let chunks_json = serde_json::to_value(chunks).unwrap_or(Value::Array(Vec::new()));
        let result = match persistence {
            StreamPersistence::Postgres(pool) => match (request_id.parse::<i64>(), project_id.parse::<i64>()) {
                (Ok(request_id), Ok(project_id)) => sqlx::query(
                    "UPDATE requests SET response_chunks=$1::jsonb,updated_at=CURRENT_TIMESTAMP WHERE id=$2 AND project_id=$3",
                ).bind(chunks_json)
                    .bind(request_id).bind(project_id).execute(pool).await.map(|_|()),
                _ => return Ok(()),
            },
        };
        if let Err(error) = result {
            warn!(%error, request_id, "stream recorder: failed to save request chunks");
        }
        Ok(())
    }
}

impl UsageLogRecorder {
    async fn persist_route_explanation(
        &self,
        ctx: &OrchestratorContext,
        request_id: &str,
        project_id: &str,
        final_channel_id: Option<&str>,
        final_model_id: Option<&str>,
        terminal_error: Option<&str>,
    ) {
        let (Ok(request_id), Ok(project_id)) =
            (request_id.parse::<i64>(), project_id.parse::<i64>())
        else {
            return;
        };
        let final_channel_id = final_channel_id.and_then(|value| value.parse::<i64>().ok());
        let diagnostics: Value = ctx
            .metadata
            .get("route_selection_diagnostics")
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| serde_json::json!({"selected": [], "rejected": []}));
        let selected = diagnostics
            .get("selected")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let rejected = diagnostics
            .get("rejected")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let ordered: Value = ctx
            .metadata
            .get("route_ordered_candidates")
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_else(|| Value::Array(Vec::new()));
        let requested_model = ctx
            .metadata
            .get("route_requested_model")
            .cloned()
            .unwrap_or_default();
        let strategy = ctx
            .metadata
            .get(conduit_orchestrator::orchestrator::LOAD_BALANCE_STRATEGY_METADATA)
            .cloned()
            .unwrap_or_else(|| "system_default".to_owned());
        let affinity_key_class = ctx
            .metadata
            .get(conduit_orchestrator::orchestrator::ROUTE_AFFINITY_APPLIED_CLASS_METADATA)
            .or_else(|| {
                ctx.metadata
                    .get(conduit_orchestrator::orchestrator::ROUTE_AFFINITY_KEY_CLASS_METADATA)
            });
        let affinity_decision = ctx
            .metadata
            .get(conduit_orchestrator::orchestrator::ROUTE_AFFINITY_DECISION_METADATA);

        let Some(persistence) = &self.stream_persistence else {
            return;
        };
        let result = match persistence {
            StreamPersistence::Postgres(pool) => sqlx::query(
                "INSERT INTO request_route_explanations \
                 (request_id,project_id,requested_model,load_balance_strategy,selected_candidates,rejected_candidates,ordered_candidates,final_channel_id,final_model_id,terminal_error,affinity_key_class,affinity_decision) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT(request_id) DO UPDATE SET \
                 selected_candidates=EXCLUDED.selected_candidates,rejected_candidates=EXCLUDED.rejected_candidates,ordered_candidates=EXCLUDED.ordered_candidates,final_channel_id=EXCLUDED.final_channel_id,final_model_id=EXCLUDED.final_model_id,terminal_error=EXCLUDED.terminal_error,affinity_key_class=EXCLUDED.affinity_key_class,affinity_decision=EXCLUDED.affinity_decision,updated_at=now()",
            )
            .bind(request_id)
            .bind(project_id)
            .bind(requested_model)
            .bind(strategy)
            .bind(selected)
            .bind(rejected)
            .bind(ordered)
            .bind(final_channel_id)
            .bind(final_model_id)
            .bind(terminal_error)
            .bind(affinity_key_class)
            .bind(affinity_decision)
            .execute(pool)
            .await
            .map(|_| ()),
        };
        if let Err(error) = result {
            warn!(%error, request_id, "route explanation persistence failed");
        }
    }

    async fn release_reservation(&self, ctx: &OrchestratorContext, reason: &str) {
        let (Some(settler), Some(key)) = (
            self.charge_settler.as_ref(),
            ctx.metadata.get("billing_reservation_key"),
        ) else {
            return;
        };
        if let Err(error) = settler.release_request(key, reason).await {
            warn!(%error, reservation_key = %key, "wallet reservation release failed");
        }
    }
}

/// Convert the services-layer [`UsageLog`] into the db-layer
/// [`CreateUsageLogInput`]. The row's own `id` is left empty — the repository
/// inserts with `RETURNING id` (DB autoincrement) and ignores any caller id
/// (the connection-pool `last_insert_rowid` reliability note in the request
/// repo applies to usage logs too).
fn usage_log_to_create_input(log: UsageLog) -> CreateUsageLogInput {
    // Compute the fields that partially move / borrow `log` up front so the
    // struct literal below stays a set of disjoint field moves.
    let source = log.source.as_str().to_string();
    let total_cost = log
        .total_cost
        .and_then(|cost| cost.to_string().parse::<f64>().ok());
    let cost_items =
        serde_json::to_value(&log.cost_items).unwrap_or_else(|_| Value::Array(Vec::new()));

    CreateUsageLogInput {
        id: String::new(),
        project_id: log.project_id.to_string(),
        request_id: log.request_id.to_string(),
        api_key_id: log.api_key_id.map(|id| id.to_string()),
        channel_id: log.channel_id.map(|id| id.to_string()),
        model_id: log.model_id,
        prompt_tokens: log.prompt_tokens,
        completion_tokens: log.completion_tokens,
        total_tokens: log.total_tokens,
        prompt_audio_tokens: log.prompt_audio_tokens,
        prompt_cached_tokens: log.prompt_cached_tokens,
        prompt_write_cached_tokens: log.prompt_write_cached_tokens,
        prompt_write_cached_tokens_5m: log.prompt_write_cached_tokens_5m,
        prompt_write_cached_tokens_1h: log.prompt_write_cached_tokens_1h,
        completion_audio_tokens: log.completion_audio_tokens,
        completion_reasoning_tokens: log.completion_reasoning_tokens,
        completion_accepted_prediction_tokens: log.completion_accepted_prediction_tokens,
        completion_rejected_prediction_tokens: log.completion_rejected_prediction_tokens,
        source,
        format: log.format,
        total_cost,
        cost_items,
        cost_price_reference_id: log.cost_price_reference_id,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::MemoryCache;
    use conduit_core::objects::money::CurrencyExchangeRate;
    use conduit_db::repo::usage_repo::InMemoryUsageRepo;
    use conduit_db::{InMemoryRouteAffinityRepo, RouteAffinityKey, RouteAffinityRepo, UsageLogRow};
    use conduit_llm::Usage;
    use conduit_pipeline::pipeline::{AttemptRecord, ExecutionMode};

    #[derive(Default)]
    struct CapturingChargeSettler {
        releases: Mutex<Vec<(String, String)>>,
        released: tokio::sync::Notify,
    }

    #[async_trait]
    impl crate::usage_charge_settler::UsageChargeSettler for CapturingChargeSettler {
        async fn settle_usage(
            &self,
            _usage_log: &UsageLogRow,
            _usage: &Usage,
            _reservation_key: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn release_request(&self, reservation_key: &str, reason: &str) -> Result<(), String> {
            self.releases
                .lock()
                .map_err(|_| "release capture lock poisoned".to_string())?
                .push((reservation_key.to_string(), reason.to_string()));
            self.released.notify_one();
            Ok(())
        }
    }

    /// A succeeded attempt against `channel` (mirrors the orchestrator recorder
    /// test helper).
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
        HttpResponse {
            status: 200,
            usage: Some(Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                total_tokens: prompt + completion,
                ..Usage::default()
            }),
            ..HttpResponse::default()
        }
    }

    #[tokio::test]
    async fn successful_attempt_overwrites_negative_sticky_channel_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = Arc::new(MemoryCache::new(std::time::Duration::from_secs(60)));
        let cache_key = crate::route_affinity::sticky_channel_cache_key("77");
        let negative = crate::route_affinity::sticky_channel_cache_value(
            None,
            chrono::Utc::now(),
            std::time::Duration::from_secs(5),
        )
        .expect("negative sticky cache value");
        cache.set(&cache_key, negative, None).await?;
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()))
            .with_sticky_channel_cache(cache.clone());
        let mut ctx = OrchestratorContext::new();
        ctx.metadata.insert("trace_id".into(), "77".into());

        recorder
            .record_success(
                &ctx,
                "1",
                "1",
                &succeeded_attempt(1, "12"),
                &HttpResponse::default(),
            )
            .await?;

        let cached = cache.get(&cache_key).await?.expect("sticky cache value");
        assert_eq!(
            crate::route_affinity::decode_sticky_channel_cache(cached, chrono::Utc::now())?,
            crate::route_affinity::StickyChannelCacheState::Fresh(Some("12".into()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn successful_response_persists_prompt_key_and_response_id_route_affinity()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRouteAffinityRepo::new());
        let runtime = Arc::new(crate::route_affinity::RouteAffinityRuntime::new(
            repo.clone(),
            Arc::new(MemoryCache::new(std::time::Duration::from_secs(60))),
            crate::route_affinity::RouteAffinityRuntimeConfig {
                prompt_cache_ttl: std::time::Duration::from_secs(3600),
                response_continuity_ttl: std::time::Duration::from_secs(7200),
                lookup_cache_ttl: std::time::Duration::from_secs(60),
                negative_cache_ttl: std::time::Duration::from_secs(5),
            },
        ));
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()))
            .with_route_affinity_runtime(Some(runtime));
        let mut ctx = OrchestratorContext::new();
        let prompt_hash = crate::route_affinity::hash_explicit_affinity_value("raw-prompt-key");
        let credential_identity = conduit_services::credential_fingerprint("credential");
        for (key, value) in [
            (
                conduit_orchestrator::orchestrator::ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
                "gpt-public",
            ),
            (
                conduit_orchestrator::orchestrator::ROUTE_AFFINITY_API_FORMAT_METADATA,
                "openai/responses",
            ),
            (
                conduit_orchestrator::orchestrator::ROUTE_AFFINITY_PROMPT_CACHE_HASH_METADATA,
                prompt_hash.as_str(),
            ),
            ("actual_model", "gpt-upstream"),
            ("format", "openai/responses"),
            ("credential_identity", credential_identity.as_str()),
        ] {
            ctx.metadata.insert(key.into(), value.into());
        }
        let response = HttpResponse {
            status: 200,
            json_body: Some(json!({"id": "resp_raw_123", "model": "gpt-upstream"})),
            ..HttpResponse::default()
        };

        recorder
            .record_success(&ctx, "1", "7", &succeeded_attempt(1, "12"), &response)
            .await?;

        let svc_ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let lookup = |key_class: &str, key_hash: String| RouteAffinityKey {
            project_id: "7".into(),
            key_class: key_class.into(),
            key_hash,
            public_model_id: "gpt-public".into(),
            api_format: "openai/responses".into(),
        };
        let prompt = repo
            .find_valid_route_affinity(
                &svc_ctx,
                &lookup(conduit_db::KEY_CLASS_PROMPT_CACHE_KEY, prompt_hash),
                chrono::Utc::now(),
            )
            .await?
            .expect("prompt route affinity");
        let response_route = repo
            .find_valid_route_affinity(
                &svc_ctx,
                &lookup(
                    conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID,
                    crate::route_affinity::hash_explicit_affinity_value("resp_raw_123"),
                ),
                chrono::Utc::now(),
            )
            .await?
            .expect("response route affinity");

        for row in [prompt, response_route] {
            assert_eq!(row.channel_id, "12");
            assert_eq!(row.upstream_model_id, "gpt-upstream");
            assert_eq!(row.upstream_api_format, "openai/responses");
            assert_eq!(
                row.credential_identity.as_deref(),
                Some(credential_identity.as_str())
            );
            assert!(!row.key_hash.contains("raw-prompt-key"));
            assert!(!row.key_hash.contains("resp_raw_123"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn initial_responses_success_seeds_returned_response_id_affinity()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryRouteAffinityRepo::new());
        let runtime = Arc::new(crate::route_affinity::RouteAffinityRuntime::new(
            repo.clone(),
            Arc::new(MemoryCache::new(std::time::Duration::from_secs(60))),
            crate::route_affinity::RouteAffinityRuntimeConfig {
                prompt_cache_ttl: std::time::Duration::from_secs(3600),
                response_continuity_ttl: std::time::Duration::from_secs(7200),
                lookup_cache_ttl: std::time::Duration::from_secs(60),
                negative_cache_ttl: std::time::Duration::from_secs(5),
            },
        ));
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()))
            .with_route_affinity_runtime(Some(runtime));
        let mut ctx = OrchestratorContext::new();
        for (key, value) in [
            (
                conduit_orchestrator::orchestrator::ROUTE_AFFINITY_PUBLIC_MODEL_METADATA,
                "gpt-public",
            ),
            (
                conduit_orchestrator::orchestrator::ROUTE_AFFINITY_API_FORMAT_METADATA,
                "openai/responses",
            ),
            ("actual_model", "gpt-upstream"),
            ("format", "openai/responses"),
            ("credential_identity", "raw-provider-secret"),
        ] {
            ctx.metadata.insert(key.into(), value.into());
        }

        recorder
            .record_success(
                &ctx,
                "1",
                "7",
                &succeeded_attempt(1, "12"),
                &HttpResponse {
                    status: 200,
                    json_body: Some(json!({"id": "resp_initial"})),
                    ..HttpResponse::default()
                },
            )
            .await?;

        let found = repo
            .find_valid_route_affinity(
                &RequestContext::new(PolicyContext::new(Principal::test())),
                &RouteAffinityKey {
                    project_id: "7".into(),
                    key_class: conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID.into(),
                    key_hash: crate::route_affinity::hash_explicit_affinity_value("resp_initial"),
                    public_model_id: "gpt-public".into(),
                    api_format: "openai/responses".into(),
                },
                chrono::Utc::now(),
            )
            .await?;

        assert_eq!(
            found.as_ref().map(|row| row.channel_id.as_str()),
            Some("12")
        );
        assert_eq!(
            found.and_then(|row| row.credential_identity),
            None,
            "invalid credential metadata must never be persisted"
        );
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn api_key_concurrency_isolated_by_key_and_released_on_terminal_paths() {
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()));
        let mut first = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut first, 7, 1)
            .expect("first slot");
        let mut other_key = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut other_key, 8, 1)
            .expect("a different key has an independent slot");
        let mut second_same_key = OrchestratorContext::new();
        let error = recorder
            .acquire_api_key_slot(&mut second_same_key, 7, 1)
            .expect_err("second request must be rejected");
        assert!(error.to_string().contains("concurrency limit exceeded"));

        recorder
            .record_failure(
                &first,
                "1",
                "1",
                &ConduitError::internal("terminal failure"),
            )
            .await
            .expect("failure finalization");
        recorder
            .acquire_api_key_slot(&mut second_same_key, 7, 1)
            .expect("slot reusable after failure release");

        recorder
            .record_success(
                &second_same_key,
                "1",
                "1",
                &succeeded_attempt(1, "1"),
                &response_with_usage(1, 1),
            )
            .await
            .expect("success finalization");
        let mut after_success = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut after_success, 7, 1)
            .expect("slot reusable after success release");

        let mut still_blocked = OrchestratorContext::new();
        assert!(
            recorder
                .acquire_api_key_slot(&mut still_blocked, 8, 1)
                .is_err(),
            "releasing key 7 must not affect key 8"
        );
        recorder
            .record_failure(
                &other_key,
                "2",
                "1",
                &ConduitError::internal("terminal failure"),
            )
            .await
            .expect("other-key failure finalization");
        recorder
            .acquire_api_key_slot(&mut still_blocked, 8, 1)
            .expect("other key released independently");
    }

    #[test]
    fn duplicate_api_key_release_does_not_free_another_request_lease() {
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()));
        let mut first = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut first, 7, 1)
            .expect("first lease");
        recorder.release_api_key_slot(&first);

        let mut second = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut second, 7, 1)
            .expect("second lease after first release");

        // A timeout finalizer can race a recorder callback for the first
        // request. Replaying that first release must not decrement the active
        // second request.
        recorder.release_api_key_slot(&first);
        let mut third = OrchestratorContext::new();
        assert!(
            recorder.acquire_api_key_slot(&mut third, 7, 1).is_err(),
            "duplicate release of the first lease must not free the second lease"
        );

        recorder.release_api_key_slot(&second);
        recorder
            .acquire_api_key_slot(&mut third, 7, 1)
            .expect("slot is reusable after releasing the active lease");
    }

    #[tokio::test]
    async fn abandoned_request_releases_slot_and_schedules_wallet_cleanup()
    -> Result<(), Box<dyn std::error::Error>> {
        let settler = Arc::new(CapturingChargeSettler::default());
        let recorder = UsageLogRecorder::new(Arc::new(InMemoryUsageRepo::new()))
            .with_charge_settler(settler.clone());
        let mut abandoned = OrchestratorContext::new();
        abandoned.metadata.insert(
            conduit_orchestrator::orchestrator::BILLING_ADMISSION_REQUEST_KEY_METADATA.to_string(),
            "request-canceled".to_string(),
        );
        recorder.acquire_api_key_slot(&mut abandoned, 7, 1)?;

        let released = settler.released.notified();
        recorder.abandon_request(&abandoned, "outer timeout");

        let mut replacement = OrchestratorContext::new();
        recorder
            .acquire_api_key_slot(&mut replacement, 7, 1)
            .expect("in-memory slot must be available synchronously");
        tokio::time::timeout(std::time::Duration::from_secs(1), released).await?;
        assert_eq!(
            settler
                .releases
                .lock()
                .map_err(|_| "release capture lock poisoned")?
                .as_slice(),
            &[("request-canceled".to_string(), "outer timeout".to_string())]
        );
        Ok(())
    }

    #[test]
    fn stream_chunks_serialize_as_json_array() {
        let chunks = vec![conduit_llm::StreamEvent {
            event_type: Some("message".to_string()),
            json_data: Some(serde_json::json!({"delta": "hello"})),
            ..conduit_llm::StreamEvent::default()
        }];

        let stored = stream_chunks_value(&chunks);
        assert!(stored.is_array());
        assert_eq!(stored[0]["json_data"]["delta"], "hello");
    }

    #[tokio::test]
    async fn record_success_writes_one_usage_row() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let mut ctx = OrchestratorContext::new();
        ctx.metadata
            .insert("actual_model".to_string(), "gpt-4o".to_string());
        let attempt = succeeded_attempt(1, "7");
        let response = response_with_usage(50, 150);

        recorder
            .record_success(&ctx, "1", "1", &attempt, &response)
            .await?;

        assert_eq!(repo.len()?, 1, "exactly one usage-log row expected");
        Ok(())
    }

    /// P-44: when the orchestrator context carries `api_key_id`, the recorded
    /// usage_log row must be attributed to it (was hard-coded to None, which
    /// made every non-RPM per-key quota count zero rows and never fire).
    #[tokio::test]
    async fn record_success_attributes_api_key_id_from_context()
    -> Result<(), Box<dyn std::error::Error>> {
        use conduit_db::repo::usage_repo::{UsageListQuery, UsageRepo};

        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let mut ctx = OrchestratorContext::new();
        ctx.metadata
            .insert("actual_model".to_string(), "gpt-4o".to_string());
        // The identity key the orchestrator copies from the request metadata.
        ctx.metadata
            .insert("api_key_id".to_string(), "42".to_string());
        let attempt = succeeded_attempt(1, "7");
        let response = response_with_usage(50, 150);

        recorder
            .record_success(&ctx, "1", "1", &attempt, &response)
            .await?;

        let admin = RequestContext::new(PolicyContext::new(conduit_db::Principal::system()));
        let query = UsageListQuery {
            project_id: "1".to_string(),
            limit: 100,
            ..UsageListQuery::default()
        };
        let rows = repo.list_usage(&admin, &query).await?;
        let row = rows.rows.first().ok_or("expected one usage_log row")?;
        assert_eq!(
            row.api_key_id.as_deref(),
            Some("42"),
            "usage_log must be attributed to the context's api_key_id"
        );
        Ok(())
    }

    /// A non-numeric api_key_id in context is logged and recorded as NULL
    /// rather than crashing or being silently mis-attributed.
    #[tokio::test]
    async fn record_success_records_null_for_unparseable_api_key_id()
    -> Result<(), Box<dyn std::error::Error>> {
        use conduit_db::repo::usage_repo::{UsageListQuery, UsageRepo};

        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let mut ctx = OrchestratorContext::new();
        ctx.metadata
            .insert("actual_model".to_string(), "gpt-4o".to_string());
        ctx.metadata
            .insert("api_key_id".to_string(), "not-a-number".to_string());
        let attempt = succeeded_attempt(1, "7");
        let response = response_with_usage(50, 150);

        recorder
            .record_success(&ctx, "1", "1", &attempt, &response)
            .await?;

        let admin = RequestContext::new(PolicyContext::new(conduit_db::Principal::system()));
        let query = UsageListQuery {
            project_id: "1".to_string(),
            limit: 100,
            ..UsageListQuery::default()
        };
        let rows = repo.list_usage(&admin, &query).await?;
        let row = rows.rows.first().ok_or("expected one usage_log row")?;
        assert_eq!(row.api_key_id, None, "unparseable id → NULL, not a crash");
        Ok(())
    }

    #[tokio::test]
    async fn record_success_skips_when_usage_zero() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "7");
        let response = HttpResponse {
            usage: Some(Usage::default()), // zero usage
            ..HttpResponse::default()
        };

        recorder
            .record_success(&ctx, "1", "1", &attempt, &response)
            .await?;

        assert_eq!(repo.len()?, 0, "zero usage must not write a row");
        Ok(())
    }

    #[tokio::test]
    async fn record_success_skips_when_no_usage_field() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "7");
        let response = HttpResponse::default(); // no usage

        recorder
            .record_success(&ctx, "1", "1", &attempt, &response)
            .await?;

        assert_eq!(repo.len()?, 0, "missing usage must not write a row");
        Ok(())
    }

    #[tokio::test]
    async fn record_success_skips_on_non_numeric_ids() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let ctx = OrchestratorContext::new();
        let attempt = succeeded_attempt(1, "7");
        let response = response_with_usage(10, 20);

        // Non-numeric request id -> row dropped (Go never sees this; defensive).
        recorder
            .record_success(&ctx, "not-a-number", "1", &attempt, &response)
            .await?;
        assert_eq!(repo.len()?, 0, "non-numeric request id must write no row");
        Ok(())
    }

    #[tokio::test]
    async fn record_failure_is_a_noop() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(InMemoryUsageRepo::new());
        let recorder = UsageLogRecorder::new(repo.clone());

        let ctx = OrchestratorContext::new();
        let err = ConduitError::upstream("provider 500");
        recorder.record_failure(&ctx, "1", "1", &err).await?;

        assert_eq!(repo.len()?, 0, "record_failure must not write a usage row");
        Ok(())
    }

    #[test]
    fn resolve_model_prefers_actual_model() {
        let mut ctx = OrchestratorContext::new();
        assert_eq!(UsageLogRecorder::resolve_model(&ctx), "");

        ctx.metadata
            .insert("original_model".to_string(), "gpt-3.5".to_string());
        assert_eq!(UsageLogRecorder::resolve_model(&ctx), "gpt-3.5");

        ctx.metadata
            .insert("actual_model".to_string(), "gpt-4o".to_string());
        assert_eq!(
            UsageLogRecorder::resolve_model(&ctx),
            "gpt-4o",
            "actual_model takes precedence"
        );
    }

    #[test]
    fn conversion_maps_token_counts() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 200,
            total_tokens: 300,
            ..Usage::default()
        };
        let params = CreateUsageLogParams::new(
            1,
            2,
            Some(42),
            "gpt-4o",
            &usage,
            UsageLogSource::Api,
            DEFAULT_FORMAT,
            Some(7),
        );
        let log = create_usage_log_from_structured_usage(params);
        let input = usage_log_to_create_input(log);

        assert_eq!(input.request_id, "1");
        assert_eq!(input.project_id, "2");
        assert_eq!(input.channel_id.as_deref(), Some("42"));
        assert_eq!(input.api_key_id.as_deref(), Some("7"));
        assert_eq!(input.model_id, "gpt-4o");
        assert_eq!(input.prompt_tokens, 100);
        assert_eq!(input.completion_tokens, 200);
        assert_eq!(input.total_tokens, 300);
        assert_eq!(input.source, "api");
        assert_eq!(input.format, DEFAULT_FORMAT);
        // `id` is intentionally empty (DB autoincrement via RETURNING id).
        assert!(input.id.is_empty());
    }

    /// A resolved per-channel price flows through `ComputeUsageCost` into a
    /// non-zero `total_cost` on the created input — proving the billing wiring
    /// (price → cost → row) works. The `ModelPrice` is built from the same JSON
    /// shape `resolve_price` deserializes (usage_per_unit @ $1/1k prompt tokens),
    /// so this exercises the production deserialization path too.
    #[test]
    fn resolved_price_produces_non_zero_cost() -> Result<(), Box<dyn std::error::Error>> {
        // `usagePerUnit` is a bare Decimal string = price per **million** tokens
        // (compute_item_subtotal: per_unit * quantity/1e6). $10/1M prompt +
        // $30/1M completion.
        let price_json = serde_json::json!({
            "items": [
                {
                    "itemCode": "prompt_tokens",
                    "pricing": { "mode": "usage_per_unit", "usagePerUnit": "10.0" }
                },
                {
                    "itemCode": "completion_tokens",
                    "pricing": { "mode": "usage_per_unit", "usagePerUnit": "30.0" }
                }
            ]
        });
        let price: ModelPrice = serde_json::from_value(price_json)?;

        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            ..Usage::default()
        };
        let params = CreateUsageLogParams::new(
            1,
            1,
            Some(7),
            "gpt-4o",
            &usage,
            UsageLogSource::Api,
            DEFAULT_FORMAT,
            None,
        )
        .with_resolved_price(ResolvedModelPrice {
            price: &price,
            reference_id: "ref-1",
        });
        let log = create_usage_log_from_structured_usage(params);
        let input = usage_log_to_create_input(log);

        // 1000/1e6*10 + 1000/1e6*30 = 0.04 — the key assertion: cost is NOT
        // the no-cost fallback (0) once a price is resolved.
        match input.total_cost {
            Some(cost) => assert!(cost > 0.0, "expected non-zero cost, got {cost}"),
            None => return Err("expected Some(total_cost) when price resolved".into()),
        }
        assert_eq!(input.cost_price_reference_id.as_deref(), Some("ref-1"));
        Ok(())
    }

    #[test]
    fn import_cost_is_converted_and_audited_in_accounting_currency()
    -> Result<(), Box<dyn std::error::Error>> {
        let price: ModelPrice = serde_json::from_value(serde_json::json!({
            "items": [{
                "itemCode": "prompt_tokens",
                "pricing": { "mode": "usage_per_unit", "usagePerUnit": "10" }
            }]
        }))?;
        let usage = Usage {
            prompt_tokens: 1_000_000,
            total_tokens: 1_000_000,
            ..Usage::default()
        };
        let source = create_usage_log_from_structured_usage(
            CreateUsageLogParams::new(
                1,
                1,
                Some(7),
                "gpt-4o",
                &usage,
                UsageLogSource::Api,
                DEFAULT_FORMAT,
                None,
            )
            .with_resolved_price(ResolvedModelPrice {
                price: &price,
                reference_id: "ref-usd",
            }),
        );
        let settings = AccountingSettings {
            accounting_currency: "CNY".into(),
            exchange_rates: vec![CurrencyExchangeRate {
                currency: "USD".into(),
                // 1 CNY = 0.2 USD, so USD 10 = CNY 50.
                quote_per_accounting_unit: Decimal::new(2, 1),
            }],
            version: 3,
            ..Default::default()
        };
        let (converted, audit) = convert_import_cost_to_accounting(source, "usd", Ok(settings));
        let mut input = usage_log_to_create_input(converted);
        audit.attach_to_cost_items(&mut input.cost_items);

        assert_eq!(input.total_cost, Some(50.0));
        assert_eq!(input.cost_items[0]["subtotal"], "50");
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["sourceSubtotal"],
            "10"
        );
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["sourceCurrency"],
            "USD"
        );
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["accountingCurrency"],
            "CNY"
        );
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["accountingSettingsVersion"],
            3
        );
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["status"],
            "converted"
        );
        Ok(())
    }

    #[test]
    fn missing_fx_keeps_source_audit_but_never_records_mixed_currency_total()
    -> Result<(), Box<dyn std::error::Error>> {
        let price: ModelPrice = serde_json::from_value(serde_json::json!({
            "items": [{
                "itemCode": "prompt_tokens",
                "pricing": { "mode": "usage_per_unit", "usagePerUnit": "10" }
            }]
        }))?;
        let usage = Usage {
            prompt_tokens: 1_000_000,
            total_tokens: 1_000_000,
            ..Usage::default()
        };
        let source = create_usage_log_from_structured_usage(
            CreateUsageLogParams::new(
                1,
                1,
                Some(7),
                "gpt-4o",
                &usage,
                UsageLogSource::Api,
                DEFAULT_FORMAT,
                None,
            )
            .with_resolved_price(ResolvedModelPrice {
                price: &price,
                reference_id: "ref-usd",
            }),
        );
        let (converted, audit) =
            convert_import_cost_to_accounting(source, "USD", Ok(AccountingSettings::default()));
        let mut input = usage_log_to_create_input(converted);
        audit.attach_to_cost_items(&mut input.cost_items);

        assert_eq!(input.total_cost, None);
        assert_eq!(input.cost_items[0]["subtotal"], "10");
        assert_eq!(
            input.cost_items[0]["accountingConversion"]["status"],
            "failed"
        );
        assert!(
            input.cost_items[0]["accountingConversion"]["error"]
                .as_str()
                .is_some_and(|error| error.contains("missing exchange rate for USD"))
        );
        Ok(())
    }

    #[test]
    fn request_flat_fee_is_recognized_without_token_usage() -> Result<(), Box<dyn std::error::Error>>
    {
        let request_price: ModelPrice = serde_json::from_value(serde_json::json!({
            "items": [{
                "itemCode": "request",
                "pricing": { "mode": "flat_fee", "flatFee": "0.05" }
            }]
        }))?;
        let token_price: ModelPrice = serde_json::from_value(serde_json::json!({
            "items": [{
                "itemCode": "prompt_tokens",
                "pricing": { "mode": "usage_per_unit", "usagePerUnit": "10" }
            }]
        }))?;
        let zero_flat_price: ModelPrice = serde_json::from_value(serde_json::json!({
            "items": [{
                "itemCode": "request",
                "pricing": { "mode": "flat_fee", "flatFee": "0" }
            }]
        }))?;

        assert!(UsageLogRecorder::has_request_flat_fee(&request_price));
        assert!(!UsageLogRecorder::has_request_flat_fee(&token_price));
        assert!(!UsageLogRecorder::has_request_flat_fee(&zero_flat_price));
        Ok(())
    }

    /// Without a resolved price (the `None` path), `total_cost` stays at the
    /// no-cost fallback so token accounting still records but no cost is billed.
    #[test]
    fn no_price_leaves_cost_unset() -> Result<(), Box<dyn std::error::Error>> {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            ..Usage::default()
        };
        let params = CreateUsageLogParams::new(
            1,
            1,
            Some(7),
            "gpt-4o",
            &usage,
            UsageLogSource::Api,
            DEFAULT_FORMAT,
            None,
        );
        let log = create_usage_log_from_structured_usage(params);
        let input = usage_log_to_create_input(log);
        // No-cost fallback: total_cost is None or 0 (never a positive charge).
        match input.total_cost {
            None => {}
            Some(cost) => assert_eq!(cost, 0.0, "no price must not bill a positive cost"),
        }
        Ok(())
    }
}

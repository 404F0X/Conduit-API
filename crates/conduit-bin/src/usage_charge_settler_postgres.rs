//! PostgreSQL customer usage settlement with transactional, idempotent debits.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_core::objects::money::{AccountingSettings, CurrencyExchangeRate, STATION_CREDIT_CODE};
use conduit_core::objects::pricing::ModelPrice;
use conduit_db::row::UsageLogRow;
use conduit_llm::Usage;
use conduit_services::usage_service::compute_usage_cost_full;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore, mpsc};
use tracing::warn;

use conduit_orchestrator::orchestrator::BillingAdmissionInput;

use crate::usage_charge_settler::{BillingEnforcementMode, UsageChargeSettler, usage_from_row};

#[derive(Clone)]
pub(crate) struct PgUsageChargeSettler {
    pool: PgPool,
    enforcement_mode: BillingEnforcementMode,
    wallet_gates: Arc<WalletAdmissionGates>,
    settlement_tx: mpsc::Sender<SettlementJob>,
    async_settlement: bool,
}

struct SettlementJob {
    log: UsageLogRow,
    usage: Usage,
    reservation_key: Option<String>,
}

#[derive(Default, serde::Deserialize)]
struct MoneySettingsWire {
    accounting_currency_code: Option<String>,
    credit_display_name: Option<String>,
    credits_per_accounting_unit: Option<Decimal>,
    #[serde(default)]
    exchange_rates: Vec<CurrencyExchangeRate>,
    accounting_rate_version: Option<u64>,
}

pub(crate) async fn load_accounting_settings(pool: &PgPool) -> Result<AccountingSettings, String> {
    let raw = sqlx::query_scalar::<_, String>(
        "SELECT value FROM systems WHERE key='system_general_settings' AND deleted_at=0 LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let wire = match raw {
        Some(raw) => serde_json::from_str::<MoneySettingsWire>(&raw)
            .map_err(|error| format!("invalid accounting settings: {error}"))?,
        None => MoneySettingsWire::default(),
    };
    let mut settings = AccountingSettings::default();
    if let Some(value) = wire
        .accounting_currency_code
        .filter(|value| !value.trim().is_empty())
    {
        settings.accounting_currency = value.trim().to_ascii_uppercase();
    }
    if let Some(value) = wire
        .credit_display_name
        .filter(|value| !value.trim().is_empty())
    {
        settings.credit_display_name = value.trim().to_string();
    }
    if let Some(value) = wire.credits_per_accounting_unit {
        settings.credits_per_accounting_unit = value;
    }
    settings.exchange_rates = wire.exchange_rates;
    settings.version = wire.accounting_rate_version.unwrap_or(1);
    settings.validate()?;
    Ok(settings)
}

fn accounting_price_amount(
    amount: Decimal,
    price_currency: &str,
    settings: &AccountingSettings,
) -> Result<Decimal, String> {
    if price_currency.eq_ignore_ascii_case(&settings.accounting_currency) {
        return Ok(amount);
    }
    Err(format!(
        "retail price book currency {price_currency} does not match accounting currency {}",
        settings.accounting_currency
    ))
}

async fn reservation_accounting_settings(
    pool: &PgPool,
    reservation_key: Option<&str>,
) -> Result<Option<AccountingSettings>, String> {
    let Some(key) = reservation_key else {
        return Ok(None);
    };
    let detail = sqlx::query_scalar::<_, String>(
        "SELECT e.detail_snapshot FROM project_wallet_reservation_events e \
         JOIN project_wallet_reservations r ON r.id=e.reservation_id \
         WHERE r.request_id=$1 AND e.event_type IN ('reserved','shadow_reserved') \
         ORDER BY e.id DESC LIMIT 1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    detail
        .map(|value| {
            serde_json::from_str::<Value>(&value)
                .map_err(|error| format!("invalid reservation detail snapshot: {error}"))
        })
        .transpose()?
        .and_then(|value| value.get("accounting_settings").cloned())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("invalid reservation accounting snapshot: {error}"))
}

/// Keep waiters for the same wallet out of the PostgreSQL connection pool.
///
/// PostgreSQL row locks remain the cross-process correctness boundary. This
/// process-local gate only prevents a burst for one `(project, currency)` from
/// occupying every pool connection while those transactions wait for the same
/// `project_wallets ... FOR UPDATE` row.
#[derive(Default)]
struct WalletAdmissionGates {
    entries: Mutex<HashMap<(i64, String), Weak<Mutex<()>>>>,
}

#[derive(Debug, Clone, Copy)]
struct LockedWallet {
    id: i64,
    credit_balance_micros: i64,
    credit_reserved_micros: i64,
}

impl LockedWallet {
    fn available_credit(self) -> i64 {
        self.credit_balance_micros
            .saturating_sub(self.credit_reserved_micros)
            .max(0)
    }
}

#[derive(Debug, Clone, Copy)]
struct CapturingReservation {
    id: i64,
    released_credit_micros: i64,
}

#[derive(Debug, Clone, Copy)]
struct SettlementFunding {
    funded_micros: i64,
    shortfall_micros: i64,
}

impl WalletAdmissionGates {
    async fn acquire(&self, project_id: i64, currency: &str) -> OwnedMutexGuard<()> {
        let key = (project_id, currency.to_string());
        let gate = {
            let mut entries = self.entries.lock().await;
            match entries.get(&key).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    // New wallet identities are rare. Reclaim dead weak
                    // entries here so a long-lived process does not retain a
                    // key for every wallet it has ever observed.
                    entries.retain(|_, entry| entry.strong_count() > 0);
                    let gate = Arc::new(Mutex::new(()));
                    entries.insert(key, Arc::downgrade(&gate));
                    gate
                }
            }
        };
        gate.lock_owned().await
    }
}

impl PgUsageChargeSettler {
    pub(crate) fn new(pool: PgPool) -> Self {
        let queue_capacity = std::env::var("CONDUIT_BILLING_SETTLEMENT_QUEUE_CAPACITY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4096)
            .clamp(64, 65_536);
        let (settlement_tx, settlement_rx) = mpsc::channel(queue_capacity);
        let settler = Self {
            pool,
            enforcement_mode: BillingEnforcementMode::from_env(),
            wallet_gates: Arc::new(WalletAdmissionGates::default()),
            settlement_tx,
            async_settlement: !cfg!(test),
        };
        if cfg!(test) {
            drop(settlement_rx);
        } else {
            settler.start_settlement_workers(settlement_rx);
        }
        settler
    }

    fn direct_clone(&self) -> Self {
        let mut direct = self.clone();
        direct.async_settlement = false;
        direct
    }

    fn start_settlement_workers(&self, mut receiver: mpsc::Receiver<SettlementJob>) {
        let worker_count = std::env::var("CONDUIT_BILLING_SETTLEMENT_WORKERS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8)
            .clamp(1, 64);
        let permits = Arc::new(Semaphore::new(worker_count));
        let direct = self.direct_clone();
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let Ok(permit) = permits.clone().acquire_owned().await else {
                    break;
                };
                let worker = direct.clone();
                tokio::spawn(async move {
                    let usage_log_id = job.log.id.parse::<i64>().ok();
                    if let Err(error) = worker
                        .settle_usage(&job.log, &job.usage, job.reservation_key.as_deref())
                        .await
                    {
                        if let Some(usage_log_id) = usage_log_id {
                            let _ = worker.mark_outbox_failed(usage_log_id, &error).await;
                        }
                        warn!(
                            %error,
                            usage_log_id = %job.log.id,
                            "asynchronous PostgreSQL usage settlement failed; reconciler will retry"
                        );
                    } else if let Some(usage_log_id) = usage_log_id
                        && let Err(error) = worker.mark_outbox_completed(usage_log_id).await
                    {
                        warn!(
                            %error,
                            usage_log_id,
                            "settlement completed but outbox acknowledgement failed"
                        );
                    }
                    drop(permit);
                });
            }
        });
    }

    async fn persist_outbox(
        &self,
        usage_log_id: i64,
        reservation_key: Option<&str>,
    ) -> Result<bool, String> {
        let status = sqlx::query_scalar::<_, String>(
            "INSERT INTO usage_charge_outbox \
             (usage_log_id,reservation_key,status,available_at,created_at,updated_at) \
             VALUES($1,$2,'pending',now(),now(),now()) \
             ON CONFLICT(usage_log_id) DO UPDATE SET \
               reservation_key=COALESCE(usage_charge_outbox.reservation_key,EXCLUDED.reservation_key), \
               updated_at=now() RETURNING status",
        )
        .bind(usage_log_id)
        .bind(reservation_key)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        Ok(status == "completed")
    }

    async fn mark_outbox_completed(&self, usage_log_id: i64) -> Result<(), String> {
        sqlx::query(
            "UPDATE usage_charge_outbox SET status='completed',last_error=NULL,updated_at=now() \
             WHERE usage_log_id=$1",
        )
        .bind(usage_log_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    async fn mark_outbox_failed(&self, usage_log_id: i64, error: &str) -> Result<(), String> {
        sqlx::query(
            "UPDATE usage_charge_outbox SET \
             status=CASE WHEN attempts+1>=20 THEN 'failed' ELSE 'pending' END, \
             attempts=attempts+1,last_error=$2, \
             available_at=now()+interval '5 seconds',updated_at=now() \
             WHERE usage_log_id=$1 AND status<>'completed'",
        )
        .bind(usage_log_id)
        .bind(error)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn reconcile_missing(&self, limit: i64) -> Result<usize, String> {
        let limit = limit.clamp(1, 1000);
        let pending = sqlx::query(
            "SELECT usage_log_id,reservation_key FROM usage_charge_outbox \
             WHERE status='pending' AND available_at<=now() ORDER BY usage_log_id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        let repo = conduit_db::PgUsageRepo::new(self.pool.clone());
        let direct = self.direct_clone();
        let mut repaired = 0;
        for pending_row in pending {
            let id: i64 = pending_row.get("usage_log_id");
            let reservation_key: Option<String> = pending_row.get("reservation_key");
            let Some(row) = repo
                .find_by_id(id)
                .await
                .map_err(|error| error.to_string())?
            else {
                direct.mark_outbox_failed(id, "usage_log_not_found").await?;
                continue;
            };
            let usage_project_id = row.project_id.parse::<i64>().map_err(|e| e.to_string())?;
            let effective_reservation_key = if let Some(key) = reservation_key.as_deref() {
                let usage_request_id = row.request_id.parse::<i64>().map_err(|e| e.to_string())?;
                let expected_identity = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
                    "SELECT k.user_id, \
                     (SELECT m.id FROM models m WHERE m.model_id=r.model_id \
                      ORDER BY m.deleted_at,m.id DESC LIMIT 1) \
                     FROM requests r LEFT JOIN api_keys k ON k.id=r.api_key_id AND k.deleted_at=0 \
                     WHERE r.id=$1",
                )
                .bind(usage_request_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| error.to_string())?;
                let reservation = sqlx::query_as::<_, (String, i64, i64, i64)>(
                    "SELECT r.status,w.project_id,r.user_id,r.public_model_id \
                     FROM project_wallet_reservations r \
                     JOIN project_wallets w ON w.id=r.wallet_id WHERE r.request_id=$1",
                )
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| error.to_string())?;
                match reservation {
                    Some((_, project_id, _, _)) if project_id != usage_project_id => {
                        direct
                            .mark_outbox_failed(id, "reservation_project_mismatch")
                            .await?;
                        continue;
                    }
                    Some((_, _, reserved_user_id, reserved_model_id))
                        if expected_identity.is_some_and(|(user_id, model_id)| {
                            user_id.is_some_and(|value| value != reserved_user_id)
                                || model_id.is_some_and(|value| value != reserved_model_id)
                        }) =>
                    {
                        direct
                            .mark_outbox_failed(id, "reservation_identity_mismatch")
                            .await?;
                        continue;
                    }
                    Some((status, _, _, _))
                        if matches!(
                            status.as_str(),
                            "expired" | "released" | "capture_failed" | "soft_insufficient"
                        ) =>
                    {
                        // The estimate hold is gone, but the persisted usage is
                        // still billable. Re-select funds atomically using the
                        // usage timestamp rather than retrying a terminal key
                        // forever.
                        None
                    }
                    Some(_) => Some(key),
                    None => None,
                }
            } else {
                None
            };
            match direct
                .settle_usage(&row, &usage_from_row(&row), effective_reservation_key)
                .await
            {
                Ok(()) => {
                    direct.mark_outbox_completed(id).await?;
                    repaired += 1;
                }
                Err(error) => direct.mark_outbox_failed(id, &error).await?,
            }
        }

        // If a process died in the narrow usage-insert/outbox-insert gap, wait
        // until the 15-minute reservation has expired before repairing without
        // a reservation key. Cleanup runs before this reconciler each cycle.
        let remaining = limit.saturating_sub(repaired as i64);
        if remaining == 0 {
            return Ok(repaired);
        }
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT ul.id FROM usage_logs ul \
             LEFT JOIN customer_charge_events c ON c.usage_log_id=ul.id \
             LEFT JOIN usage_charge_outbox o ON o.usage_log_id=ul.id \
             WHERE c.id IS NULL AND o.usage_log_id IS NULL \
               AND ul.created_at<=now()-interval '16 minutes' \
             ORDER BY ul.id LIMIT $1",
        )
        .bind(remaining)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        for id in ids {
            let Some(row) = repo.find_by_id(id).await.map_err(|e| e.to_string())? else {
                continue;
            };
            direct
                .settle_usage(&row, &usage_from_row(&row), None)
                .await?;
            repaired += 1;
        }
        Ok(repaired)
    }

    async fn cleanup_expired_reservations(&self) -> Result<usize, String> {
        let now = Utc::now();
        let wallets = sqlx::query(
            "SELECT DISTINCT w.id,w.project_id,w.currency FROM project_wallet_reservations r \
             JOIN project_wallets w ON w.id=r.wallet_id \
             WHERE r.status IN ('reserved','shadow_reserved') AND r.expires_at<=$1",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        for wallet in &wallets {
            let wallet_id: i64 = wallet.get("id");
            let project_id: i64 = wallet.get("project_id");
            let currency: String = wallet.get("currency");
            let _gate = self.wallet_gates.acquire(project_id, &currency).await;
            let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
            sqlx::query("SELECT id FROM project_wallets WHERE id=$1 FOR UPDATE")
                .bind(wallet_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
            expire_pg_wallet_reservations(&mut tx, wallet_id, now).await?;
            tx.commit().await.map_err(|e| e.to_string())?;
        }
        Ok(wallets.len())
    }

    async fn record_unsettled(
        &self,
        usage_log_id: i64,
        request_id: i64,
        public_model_id: Option<i64>,
        price_version_id: Option<i64>,
        currency: &str,
        status: &str,
        usage: &Usage,
        reason: &str,
    ) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO customer_charge_events(usage_log_id,request_id,public_model_id,price_book_version_id,amount,currency, \
             applied_rules_snapshot,usage_snapshot,calculation_snapshot,status,created_at) \
             VALUES($1,$2,$3,$4,NULL,$5,'[]'::jsonb,$6,$7,$8,$9) ON CONFLICT(usage_log_id) DO NOTHING",
        )
        .bind(usage_log_id).bind(request_id).bind(public_model_id).bind(price_version_id).bind(currency)
        .bind(sqlx::types::Json(usage)).bind(sqlx::types::Json(json!({"reason":reason})))
        .bind(status).bind(Utc::now()).execute(&self.pool).await.map_err(|e|e.to_string())?;
        Ok(())
    }
}

pub(crate) fn start_reconciler(settler: Arc<PgUsageChargeSettler>) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = settler.cleanup_expired_reservations().await {
                warn!(%error, "PostgreSQL wallet reservation expiry cleanup failed");
            }
            loop {
                match settler.reconcile_missing(100).await {
                    Ok(repaired) if repaired >= 100 => tokio::task::yield_now().await,
                    Ok(_) => break,
                    Err(error) => {
                        warn!(%error, "PostgreSQL usage charge reconciliation failed");
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

#[async_trait]
impl UsageChargeSettler for PgUsageChargeSettler {
    async fn reserve_request(
        &self,
        input: &BillingAdmissionInput,
    ) -> Result<Option<String>, String> {
        let project_id = input.project_id.parse::<i64>().map_err(|e| e.to_string())?;
        let enforcement_mode = self.enforcement_mode.for_project(project_id);
        let api_key_id = match input.api_key_id.as_deref() {
            Some(value) => value.parse::<i64>().map_err(|e| e.to_string())?,
            None => return Ok(None),
        };
        let now = Utc::now();
        let expires_at = now + chrono::Duration::minutes(15);
        let user_id = sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM api_keys WHERE id=$1 AND project_id=$2 AND deleted_at=0 LIMIT 1",
        )
        .bind(api_key_id)
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        // Resolve the public model and its active retail price in one round trip
        // on the hot path. The old implementation first selected `models.id`
        // and then queried the price book, doubling pool pressure before every
        // request had even entered its reservation transaction. Only the rare
        // error path performs the fallback identity lookup so it can preserve
        // the more specific missing-model diagnostic.
        let priced_model = sqlx::query(
            "SELECT m.id AS public_model_id,b.currency,i.price FROM models m \
             JOIN price_book_items i ON i.public_model_id=m.id \
             JOIN price_book_versions v ON v.id=i.price_book_version_id \
             JOIN price_books b ON b.id=v.price_book_id \
             WHERE m.model_id=$1 AND m.deleted_at=0 AND b.is_default=true AND b.status='enabled' \
             AND v.status='published' AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
             AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) \
             ORDER BY m.id DESC,v.version DESC LIMIT 1",
        )
        .bind(&input.public_model)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some(price_row) = priced_model else {
            let model_exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM models WHERE model_id=$1 AND deleted_at=0)",
            )
            .bind(&input.public_model)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| e.to_string())?;
            return if enforcement_mode == BillingEnforcementMode::HardEnforce {
                if model_exists {
                    Err(format!(
                        "billing admission rejected: model '{}' has no published retail price",
                        input.public_model
                    ))
                } else {
                    Err(format!(
                        "billing admission rejected: public model '{}' has no retail identity",
                        input.public_model
                    ))
                }
            } else {
                Ok(None)
            };
        };
        let public_model_id: i64 = price_row.get("public_model_id");
        let price_currency: String = price_row.get("currency");
        let price_value: Value = price_row.get("price");
        let price: ModelPrice = serde_json::from_value(price_value)
            .map_err(|e| format!("invalid retail price JSON: {e}"))?;
        let multiplier =
            crate::wiring_project_access::resolve_effective_project_price_multiplier_postgres(
                &self.pool, project_id, now,
            )
            .await
            .map_err(|e| e.to_string())?;
        let estimate = Usage {
            prompt_tokens: input.estimated_input_tokens,
            completion_tokens: input.max_output_tokens,
            total_tokens: input
                .estimated_input_tokens
                .saturating_add(input.max_output_tokens),
            ..Usage::default()
        };
        let accounting_settings = load_accounting_settings(&self.pool).await?;
        let accounting_amount = accounting_price_amount(
            compute_usage_cost_full(Some(&estimate), &price).total * multiplier,
            &price_currency,
            &accounting_settings,
        )?;
        let amount = accounting_settings
            .accounting_to_credits(accounting_amount)?
            .round_dp(6);
        let currency = STATION_CREDIT_CODE.to_string();
        let amount_micros = (amount * Decimal::from(1_000_000_i64))
            .round_dp(0)
            .to_i64()
            .ok_or_else(|| "retail reservation does not fit in i64 micros".to_string())?
            .max(0);
        if amount_micros == 0 {
            return Ok(None);
        }

        // Wait before acquiring a database connection. The wallet row lock
        // below still protects this operation across multiple Conduit API nodes.
        let _wallet_gate = self.wallet_gates.acquire(project_id, &currency).await;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        // Request keys are globally unique. Serialize one key across all
        // Conduit API instances before checking it so concurrent retries become
        // idempotent instead of racing into the unique index.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&input.request_key)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(existing) = sqlx::query(
            "SELECT r.status,r.expires_at,r.user_id,r.public_model_id,w.project_id,w.currency \
             FROM project_wallet_reservations r \
             JOIN project_wallets w ON w.id=r.wallet_id \
             WHERE r.request_id=$1",
        )
        .bind(&input.request_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        {
            let existing_project_id: i64 = existing.get("project_id");
            let existing_currency: String = existing.get("currency");
            if existing_project_id != project_id || existing_currency != currency {
                return Err(
                    "billing admission rejected: request key belongs to another project wallet"
                        .into(),
                );
            }
            if existing.get::<i64, _>("user_id") != user_id
                || existing.get::<i64, _>("public_model_id") != public_model_id
            {
                return Err(
                    "billing admission rejected: request key belongs to another user or public model"
                        .into(),
                );
            }
            let status: String = existing.get("status");
            let existing_expires_at: DateTime<Utc> = existing.get("expires_at");
            tx.commit().await.map_err(|e| e.to_string())?;
            return if matches!(status.as_str(), "reserved" | "shadow_reserved")
                && existing_expires_at > now
            {
                Ok(Some(input.request_key.clone()))
            } else if matches!(status.as_str(), "reserved" | "shadow_reserved") {
                Err("billing admission rejected: request key reservation has expired".into())
            } else {
                Err(format!(
                    "billing admission rejected: request key is already finalized with status '{status}'"
                ))
            };
        }
        // Existing wallets are overwhelmingly the hot path. Avoid issuing an
        // `INSERT .. ON CONFLICT` against the same unique index for every API
        // call; only create on the genuinely cold first request.
        let mut wallet = match lock_project_wallet(&mut tx, project_id, &currency).await? {
            Some(wallet) => wallet,
            None => {
                sqlx::query("INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) VALUES($1,$2,'active',$3,$3) ON CONFLICT(project_id,currency) DO NOTHING")
                    .bind(project_id).bind(&currency).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
                lock_project_wallet(&mut tx, project_id, &currency)
                    .await?
                    .ok_or_else(|| "project wallet could not be created".to_string())?
            }
        };
        let expired_credit = expire_pg_wallet_reservations(&mut tx, wallet.id, now).await?;
        wallet.credit_reserved_micros =
            wallet.credit_reserved_micros.saturating_sub(expired_credit);
        if enforcement_mode == BillingEnforcementMode::Shadow {
            let reservation_id = sqlx::query_scalar::<_,i64>("INSERT INTO project_wallet_reservations(wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'shadow_reserved',$6,$7,$7) RETURNING id")
                .bind(wallet.id).bind(user_id).bind(public_model_id).bind(&input.request_key).bind(amount_micros).bind(expires_at).bind(now)
                .fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
            insert_pg_reservation_event(
                &mut tx,
                reservation_id,
                "shadow_reserved",
                amount_micros,
                &input.request_key,
                json!({
                    "accounting_settings": accounting_settings,
                    "accounting_amount": accounting_amount.normalize().to_string(),
                    "credit_amount": amount.normalize().to_string(),
                    "credit_ledger_key": STATION_CREDIT_CODE,
                }),
                now,
            )
            .await?;
            tx.commit().await.map_err(|e| e.to_string())?;
            return Ok(Some(input.request_key.clone()));
        }
        let buckets = sqlx::query("SELECT b.id,b.quota_class,b.scope_snapshot,b.expires_at,GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0) available FROM subscription_allowance_buckets b \
            JOIN user_subscriptions s ON s.id=b.subscription_id JOIN subscription_plans p ON p.id=s.plan_id \
            JOIN subscription_entitlement_snapshots es ON es.id=b.entitlement_snapshot_id \
            JOIN user_subscription_projects usp ON usp.subscription_id=s.id AND usp.project_id=$1 \
            WHERE s.user_id=$2 AND s.status IN ('active','cancel_pending') AND b.status='active' AND b.period_start<=$3 AND b.expires_at>$3 AND p.currency=$4 \
            AND ((b.quota_class='GENERAL') OR (b.quota_class='DEDICATED' AND EXISTS(SELECT 1 FROM subscription_entitlement_snapshot_items esi WHERE esi.snapshot_id=es.id AND esi.quota_rule_snapshot_id=b.quota_rule_snapshot_id AND esi.public_model_id=$5))) \
            ORDER BY CASE WHEN b.quota_class='DEDICATED' THEN 0 ELSE 1 END,b.expires_at,b.id FOR UPDATE OF b")
            .bind(project_id).bind(user_id).bind(now).bind(&currency).bind(public_model_id)
            .fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
        let credit_available = wallet.available_credit();
        let subscription_available = buckets
            .iter()
            .map(|r| r.get::<i64, _>("available").max(0))
            .fold(0_i64, i64::saturating_add);
        if subscription_available.saturating_add(credit_available) < amount_micros {
            if enforcement_mode == BillingEnforcementMode::HardEnforce {
                return Err(format!(
                    "insufficient balance: estimated charge {amount_micros} micros, available {} micros",
                    subscription_available.saturating_add(credit_available)
                ));
            }
            sqlx::query("INSERT INTO project_wallet_reservations(wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'soft_insufficient',$6,$7,$7)")
                .bind(wallet.id).bind(user_id).bind(public_model_id).bind(&input.request_key).bind(amount_micros).bind(expires_at).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
            tx.commit().await.map_err(|e| e.to_string())?;
            return Ok(None);
        }
        let reservation_id = sqlx::query_scalar::<_,i64>("INSERT INTO project_wallet_reservations(wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) VALUES($1,$2,$3,$4,$5,'reserved',$6,$7,$7) RETURNING id")
            .bind(wallet.id).bind(user_id).bind(public_model_id).bind(&input.request_key).bind(amount_micros).bind(expires_at).bind(now).fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
        let mut remaining = amount_micros;
        for bucket in buckets {
            if remaining == 0 {
                break;
            }
            let bucket_id: i64 = bucket.get("id");
            let take = remaining.min(bucket.get::<i64, _>("available").max(0));
            if take == 0 {
                continue;
            }
            sqlx::query("UPDATE subscription_allowance_buckets SET reserved_micros=reserved_micros+$1,updated_at=$2 WHERE id=$3")
                .bind(take).bind(now).bind(bucket_id).execute(&mut *tx).await.map_err(|e|e.to_string())?;
            let class: String = bucket.get("quota_class");
            let scope: Value = bucket.get("scope_snapshot");
            let expiry: DateTime<Utc> = bucket.get("expires_at");
            sqlx::query("INSERT INTO project_wallet_reservation_allocations(reservation_id,source_type,source_id,amount_micros,reserved_micros,allocation_class,scope_snapshot,expires_at_snapshot,created_at) VALUES($1,'subscription_bucket',$2,$3,$3,$4,$5,$6,$7)")
                .bind(reservation_id).bind(bucket_id).bind(take).bind(class).bind(sqlx::types::Json(scope)).bind(expiry).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
            remaining -= take;
        }
        if remaining > 0 {
            sqlx::query("INSERT INTO project_wallet_reservation_allocations(reservation_id,source_type,source_id,amount_micros,reserved_micros,allocation_class,scope_snapshot,created_at) VALUES($1,'project_credit',$2,$3,$3,'PROJECT_CREDIT','{}'::jsonb,$4)")
            .bind(reservation_id).bind(wallet.id).bind(remaining).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        }
        insert_pg_reservation_event(
            &mut tx,
            reservation_id,
            "reserved",
            amount_micros,
            &input.request_key,
            json!({
                "accounting_settings": accounting_settings,
                "accounting_amount": accounting_amount.normalize().to_string(),
                "credit_amount": amount.normalize().to_string(),
                "credit_ledger_key": STATION_CREDIT_CODE,
            }),
            now,
        )
        .await?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(Some(input.request_key.clone()))
    }

    async fn release_request(&self, reservation_key: &str, reason: &str) -> Result<(), String> {
        let now = Utc::now();
        let wallet = sqlx::query(
            "SELECT w.id,w.project_id,w.currency FROM project_wallet_reservations r \
             JOIN project_wallets w ON w.id=r.wallet_id WHERE r.request_id=$1",
        )
        .bind(reservation_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some(wallet) = wallet else {
            // Do not let a reservation created after the lookup introduce a
            // reservation->wallet lock order. A later release retry will see it.
            return Ok(());
        };
        let project_id: i64 = wallet.get("project_id");
        let currency: String = wallet.get("currency");
        let _wallet_gate = self.wallet_gates.acquire(project_id, &currency).await;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        // Keep the global lock order consistent with reserve/capture:
        // wallet -> reservation -> bucket. The status trigger also updates
        // this wallet row, so locking the reservation first can deadlock with
        // capture on another Conduit API instance (the in-process gate is local).
        sqlx::query("SELECT id FROM project_wallets WHERE id=$1 FOR UPDATE")
            .bind(wallet.get::<i64, _>("id"))
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        let row = sqlx::query(
            "SELECT id,status FROM project_wallet_reservations WHERE request_id=$1 FOR UPDATE",
        )
        .bind(reservation_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        let Some(row) = row else {
            return Ok(());
        };
        let id: i64 = row.get("id");
        let status: String = row.get("status");
        if !matches!(status.as_str(), "reserved" | "shadow_reserved") {
            return Ok(());
        }
        if status == "reserved" {
            let allocations=sqlx::query("SELECT id,source_type,source_id,reserved_micros,captured_micros,released_micros FROM project_wallet_reservation_allocations WHERE reservation_id=$1")
            .bind(id).fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
            for allocation in allocations {
                let amount: i64 = allocation
                    .get::<i64, _>("reserved_micros")
                    .saturating_sub(allocation.get::<i64, _>("captured_micros"))
                    .saturating_sub(allocation.get::<i64, _>("released_micros"))
                    .max(0);
                if allocation.get::<String, _>("source_type") == "subscription_bucket" {
                    let source_id: i64 = allocation.get("source_id");
                    sqlx::query("UPDATE subscription_allowance_buckets SET \
                        reserved_micros=GREATEST(reserved_micros-$1,0), \
                        status=CASE WHEN status='draining' AND GREATEST(reserved_micros-$1,0)=0 THEN 'expired' ELSE status END, \
                        updated_at=$2 WHERE id=$3")
                    .bind(amount).bind(now).bind(source_id).execute(&mut *tx).await.map_err(|e|e.to_string())?;
                }
                sqlx::query("UPDATE project_wallet_reservation_allocations SET released_micros=released_micros+$1 WHERE id=$2")
                    .bind(amount).bind(allocation.get::<i64, _>("id"))
                    .execute(&mut *tx).await.map_err(|e| e.to_string())?;
            }
        }
        sqlx::query(
            "UPDATE project_wallet_reservations SET status='released',updated_at=$1 WHERE id=$2",
        )
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO project_wallet_reservation_events(reservation_id,event_type,amount_micros,detail_snapshot,idempotency_key,created_at) VALUES($1,'released',0,$2,$3,$4) ON CONFLICT(idempotency_key) DO NOTHING")
            .bind(id).bind(json!({"reason":reason}).to_string()).bind(format!("reservation-release:{id}")).bind(now).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn settle_usage(
        &self,
        log: &UsageLogRow,
        usage: &Usage,
        reservation_key: Option<&str>,
    ) -> Result<(), String> {
        let usage_log_id = log.id.parse::<i64>().map_err(|e| e.to_string())?;
        if self.async_settlement {
            match self.persist_outbox(usage_log_id, reservation_key).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(error) => {
                    warn!(
                        %error,
                        usage_log_id,
                        "failed to persist settlement outbox; falling back to synchronous settlement"
                    );
                    return self
                        .direct_clone()
                        .settle_usage(log, usage, reservation_key)
                        .await;
                }
            }
            let job = SettlementJob {
                log: log.clone(),
                usage: usage.clone(),
                reservation_key: reservation_key.map(str::to_owned),
            };
            if self.settlement_tx.send(job).await.is_ok() {
                return Ok(());
            }
            warn!(
                usage_log_id = %log.id,
                "PostgreSQL settlement queue is closed; falling back to synchronous settlement"
            );
        }
        let request_id = log.request_id.parse::<i64>().map_err(|e| e.to_string())?;
        let project_id = log.project_id.parse::<i64>().map_err(|e| e.to_string())?;
        let current_accounting_settings = load_accounting_settings(&self.pool).await?;
        let unsettled_currency = STATION_CREDIT_CODE.to_string();
        // Do not preflight the unique charge event. The INSERT ... ON CONFLICT
        // below is the authoritative exactly-once gate; a separate EXISTS was
        // an unavoidable extra round trip for every successful request and
        // still could not remove the race between check and insert.
        let request = sqlx::query(
            "SELECT k.user_id, \
             (SELECT m.id FROM models m WHERE m.model_id=r.model_id \
              ORDER BY m.deleted_at,m.id DESC LIMIT 1) AS public_model_id \
             FROM requests r LEFT JOIN api_keys k ON k.id=r.api_key_id AND k.deleted_at=0 WHERE r.id=$1",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| e.to_string())?;
        let Some(request) = request else {
            return self
                .record_unsettled(
                    usage_log_id,
                    request_id,
                    None,
                    None,
                    &unsettled_currency,
                    "not_billable",
                    usage,
                    "request_not_found",
                )
                .await;
        };
        let user_id: Option<i64> = request.get("user_id");
        let public_model_id: Option<i64> = request.get("public_model_id");
        let Some(public_model_id) = public_model_id else {
            return self
                .record_unsettled(
                    usage_log_id,
                    request_id,
                    None,
                    None,
                    &unsettled_currency,
                    "unpriced",
                    usage,
                    "public_model_not_found",
                )
                .await;
        };
        let Some(user_id) = user_id else {
            return self
                .record_unsettled(
                    usage_log_id,
                    request_id,
                    Some(public_model_id),
                    None,
                    &unsettled_currency,
                    "not_billable",
                    usage,
                    "request_has_no_user_api_key",
                )
                .await;
        };
        let price_row = sqlx::query(
            "SELECT v.id AS version_id,v.reference_id,b.currency,i.price FROM price_books b \
             JOIN price_book_versions v ON v.price_book_id=b.id JOIN price_book_items i ON i.price_book_version_id=v.id \
             WHERE b.is_default=TRUE AND b.status='enabled' AND v.status='published' AND i.public_model_id=$1 \
             AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
             AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) ORDER BY v.version DESC LIMIT 1",
        ).bind(public_model_id).bind(log.created_at).fetch_optional(&self.pool).await.map_err(|e|e.to_string())?;
        let Some(price_row) = price_row else {
            return self
                .record_unsettled(
                    usage_log_id,
                    request_id,
                    Some(public_model_id),
                    None,
                    &unsettled_currency,
                    "unpriced",
                    usage,
                    "published_retail_price_not_found",
                )
                .await;
        };
        let version_id: i64 = price_row.get("version_id");
        let reference: String = price_row.get("reference_id");
        let price_currency: String = price_row.get("currency");
        let price: ModelPrice = serde_json::from_value(price_row.get::<Value, _>("price"))
            .map_err(|e| format!("invalid retail price JSON: {e}"))?;
        let multiplier =
            crate::wiring_project_access::resolve_effective_project_price_multiplier_postgres(
                &self.pool,
                project_id,
                log.created_at,
            )
            .await
            .map_err(|e| e.to_string())?;
        let base = compute_usage_cost_full(Some(usage), &price);
        let accounting_settings = reservation_accounting_settings(&self.pool, reservation_key)
            .await?
            .unwrap_or(current_accounting_settings);
        let accounting_amount = accounting_price_amount(
            base.total * multiplier,
            &price_currency,
            &accounting_settings,
        )?;
        let amount = accounting_settings
            .accounting_to_credits(accounting_amount)?
            .round_dp(6);
        let currency = STATION_CREDIT_CODE.to_string();
        let amount_micros = (amount * Decimal::from(1_000_000_i64))
            .round_dp(0)
            .to_i64()
            .ok_or_else(|| "retail charge does not fit i64 micros".to_string())?
            .max(0);
        let rules =
            json!({"project_id":project_id,"price_multiplier":multiplier.normalize().to_string()});
        let calculation = json!({
            "accounting_currency_code": accounting_settings.accounting_currency.clone(),
            "price_book_unit": price_currency,
            "base_price_amount": base.total.normalize().to_string(),
            "accounting_amount": accounting_amount.normalize().to_string(),
            "final_credit_amount": amount.normalize().to_string(),
            "credit_ledger_key": STATION_CREDIT_CODE,
            "price_reference_id": reference,
            "accounting_settings": accounting_settings,
            "items": base.items
        });

        // Admission occurs before `begin()`: waiters for a hot wallet do not
        // consume scarce pool connections while another short settlement
        // transaction owns the wallet row lock.
        let _wallet_gate = self.wallet_gates.acquire(project_id, &currency).await;
        let mut tx = self.pool.begin().await.map_err(|e| e.to_string())?;
        let event_id = sqlx::query_scalar::<_,i64>(
            "INSERT INTO customer_charge_events(usage_log_id,request_id,public_model_id,price_book_version_id,amount,currency, \
             applied_rules_snapshot,usage_snapshot,calculation_snapshot,status,created_at) \
             VALUES($1,$2,$3,$4,$5::numeric,$6,$7,$8,$9,'calculated',$10) \
             ON CONFLICT(usage_log_id) DO NOTHING RETURNING id",
        ).bind(usage_log_id).bind(request_id).bind(public_model_id).bind(version_id)
            .bind(amount.normalize().to_string()).bind(&currency).bind(sqlx::types::Json(rules))
            .bind(sqlx::types::Json(usage)).bind(sqlx::types::Json(calculation)).bind(Utc::now())
            .fetch_optional(&mut *tx).await.map_err(|e|e.to_string())?;
        let Some(event_id) = event_id else {
            tx.rollback().await.map_err(|e| e.to_string())?;
            return Ok(());
        };
        // Reservation creation/expiry uses wallet -> reservation -> bucket.
        // Lock the wallet before capture touches its reservation and bucket so
        // reserve and capture cannot form a reverse-order deadlock cycle.
        let mut locked_wallet = lock_project_wallet(&mut tx, project_id, &currency).await?;
        let reservation = match (reservation_key, locked_wallet) {
            (Some(key), Some(wallet)) => {
                begin_pg_reservation_capture(
                    &mut tx,
                    key,
                    wallet.id,
                    user_id,
                    public_model_id,
                    Utc::now(),
                )
                .await?
            }
            (Some(_), None) => {
                return Err("reservation capture rejected: project wallet not found".into());
            }
            (None, _) => None,
        };
        if let (Some(wallet), Some(reservation)) = (&mut locked_wallet, reservation) {
            wallet.credit_reserved_micros = wallet
                .credit_reserved_micros
                .saturating_sub(reservation.released_credit_micros);
        }
        let funding = settle_funds_after_wallet_lock(
            &mut tx,
            event_id,
            usage_log_id,
            user_id,
            project_id,
            public_model_id,
            &currency,
            locked_wallet,
            amount_micros,
            log.created_at,
            reservation.as_ref().map(|r| r.id),
        )
        .await?;
        if let Some(reservation) = reservation {
            finish_pg_reservation_capture(
                &mut tx,
                reservation.id,
                funding.funded_micros,
                funding.shortfall_micros,
                Utc::now(),
            )
            .await?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

async fn expire_pg_wallet_reservations(
    tx: &mut Transaction<'_, Postgres>,
    wallet_id: i64,
    now: DateTime<Utc>,
) -> Result<i64, String> {
    let mut released_credit_micros = 0_i64;
    let expired=sqlx::query("SELECT id,status FROM project_wallet_reservations WHERE wallet_id=$1 AND status IN ('reserved','shadow_reserved') AND expires_at<=$2 FOR UPDATE")
        .bind(wallet_id).bind(now).fetch_all(&mut **tx).await.map_err(|e|e.to_string())?;
    for row in expired {
        let id: i64 = row.get("id");
        if row.get::<String, _>("status") == "reserved" {
            let allocations=sqlx::query("SELECT id,source_type,source_id,reserved_micros,captured_micros,released_micros FROM project_wallet_reservation_allocations WHERE reservation_id=$1")
            .bind(id).fetch_all(&mut **tx).await.map_err(|e|e.to_string())?;
            for allocation in allocations {
                let source_type: String = allocation.get("source_type");
                let amount = allocation
                    .get::<i64, _>("reserved_micros")
                    .saturating_sub(allocation.get::<i64, _>("captured_micros"))
                    .saturating_sub(allocation.get::<i64, _>("released_micros"))
                    .max(0);
                if source_type == "subscription_bucket" {
                    sqlx::query("UPDATE subscription_allowance_buckets SET \
                        reserved_micros=GREATEST(reserved_micros-$1,0), \
                        status=CASE WHEN status='draining' AND GREATEST(reserved_micros-$1,0)=0 THEN 'expired' ELSE status END, \
                        updated_at=$2 WHERE id=$3")
                .bind(amount).bind(now).bind(allocation.get::<i64,_>("source_id"))
                .execute(&mut **tx).await.map_err(|e|e.to_string())?;
                } else if source_type == "project_credit" {
                    released_credit_micros = released_credit_micros.saturating_add(amount);
                }
                sqlx::query("UPDATE project_wallet_reservation_allocations SET released_micros=released_micros+$1 WHERE id=$2")
                    .bind(amount).bind(allocation.get::<i64, _>("id"))
                    .execute(&mut **tx).await.map_err(|e| e.to_string())?;
            }
        }
        sqlx::query(
            "UPDATE project_wallet_reservations SET status='expired',updated_at=$1 WHERE id=$2",
        )
        .bind(now)
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO project_wallet_reservation_events(reservation_id,event_type,amount_micros,detail_snapshot,idempotency_key,created_at) VALUES($1,'expired',0,DEFAULT,$2,$3) ON CONFLICT(idempotency_key) DO NOTHING")
            .bind(id).bind(format!("reservation-expire:{id}")).bind(now).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    }
    Ok(released_credit_micros)
}

async fn begin_pg_reservation_capture(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    expected_wallet_id: i64,
    expected_user_id: i64,
    expected_public_model_id: i64,
    now: DateTime<Utc>,
) -> Result<Option<CapturingReservation>, String> {
    // Check immutable ownership without locking first. This prevents a bad key
    // from making us lock another project's reservation after the caller has
    // already locked the current project wallet.
    let reservation_identity = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT wallet_id,user_id,public_model_id \
         FROM project_wallet_reservations WHERE request_id=$1",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("reservation capture rejected: request key '{key}' was not found"))?;
    if reservation_identity.0 != expected_wallet_id {
        return Err(
            "reservation capture rejected: request key belongs to another project wallet".into(),
        );
    }
    if reservation_identity.1 != expected_user_id
        || reservation_identity.2 != expected_public_model_id
    {
        return Err(
            "reservation capture rejected: request key belongs to another user or public model"
                .into(),
        );
    }
    let row = sqlx::query(
        "SELECT id,status,expires_at FROM project_wallet_reservations \
         WHERE request_id=$1 AND wallet_id=$2 FOR UPDATE",
    )
    .bind(key)
    .bind(expected_wallet_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    let id: i64 = row.get("id");
    let status: String = row.get("status");
    let expires_at: DateTime<Utc> = row.get("expires_at");
    if !matches!(status.as_str(), "reserved" | "shadow_reserved") {
        return Err(format!(
            "reservation capture rejected: request key is already finalized with status '{status}'"
        ));
    }
    if expires_at <= now {
        return Err("reservation capture rejected: request key reservation has expired".into());
    }
    sqlx::query(
        "UPDATE project_wallet_reservations SET status='capturing',updated_at=$1 WHERE id=$2",
    )
    .bind(now)
    .bind(id)
    .execute(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    Ok(Some(CapturingReservation {
        id,
        released_credit_micros: 0,
    }))
}

async fn finish_pg_reservation_capture(
    tx: &mut Transaction<'_, Postgres>,
    reservation_id: i64,
    funded_micros: i64,
    shortfall_micros: i64,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let (status, event_type) = if shortfall_micros == 0 {
        ("captured", "captured")
    } else if funded_micros > 0 {
        ("partially_captured", "partially_captured")
    } else {
        ("capture_failed", "capture_failed")
    };
    sqlx::query("UPDATE project_wallet_reservations SET status=$1,settled_amount_micros=$2,updated_at=$3 WHERE id=$4")
        .bind(status).bind(funded_micros).bind(now).bind(reservation_id).execute(&mut **tx).await.map_err(|e|e.to_string())?;
    sqlx::query("INSERT INTO project_wallet_reservation_events(reservation_id,event_type,amount_micros,detail_snapshot,idempotency_key,created_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(idempotency_key) DO NOTHING")
        .bind(reservation_id).bind(event_type).bind(funded_micros)
        .bind(json!({"funded_micros":funded_micros,"shortfall_micros":shortfall_micros}).to_string())
        .bind(format!("reservation-capture:{reservation_id}")).bind(now)
        .execute(&mut **tx).await.map_err(|e|e.to_string())?;
    Ok(())
}

async fn insert_pg_reservation_event(
    tx: &mut Transaction<'_, Postgres>,
    reservation_id: i64,
    event_type: &str,
    amount_micros: i64,
    request_key: &str,
    detail_snapshot: Value,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let detail_snapshot = serde_json::to_string(&detail_snapshot)
        .map_err(|error| format!("failed to serialize reservation detail snapshot: {error}"))?;
    sqlx::query("INSERT INTO project_wallet_reservation_events(reservation_id,event_type,amount_micros,detail_snapshot,idempotency_key,created_at) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(reservation_id).bind(event_type).bind(amount_micros)
        .bind(detail_snapshot).bind(format!("reservation-{event_type}:{request_key}")).bind(now)
        .execute(&mut **tx).await.map_err(|e|e.to_string())?;
    Ok(())
}

#[cfg(test)]
async fn settle_funds(
    tx: &mut Transaction<'_, Postgres>,
    event_id: i64,
    usage_log_id: i64,
    user_id: i64,
    project_id: i64,
    public_model_id: i64,
    currency: &str,
    amount_micros: i64,
    incurred_at: DateTime<Utc>,
) -> Result<(), String> {
    let wallet = lock_project_wallet(tx, project_id, currency).await?;
    settle_funds_after_wallet_lock(
        tx,
        event_id,
        usage_log_id,
        user_id,
        project_id,
        public_model_id,
        currency,
        wallet,
        amount_micros,
        incurred_at,
        None,
    )
    .await
    .map(|_| ())
}

/// Settle after the caller has acquired the project-wallet row lock.
///
/// Reservation capture must lock wallet -> reservation -> bucket to avoid a
/// deadlock cycle. The live settlement path already owns that wallet lock
/// before capturing its reservation, so querying `FOR UPDATE` again inside
/// `settle_funds` only added a PostgreSQL round trip. Tests and standalone
/// callers retain the wrapper above, which acquires the lock once.
async fn settle_funds_after_wallet_lock(
    tx: &mut Transaction<'_, Postgres>,
    event_id: i64,
    usage_log_id: i64,
    user_id: i64,
    project_id: i64,
    public_model_id: i64,
    currency: &str,
    wallet: Option<LockedWallet>,
    amount_micros: i64,
    incurred_at: DateTime<Utc>,
    reservation_id: Option<i64>,
) -> Result<SettlementFunding, String> {
    // Capture is allocation-authoritative.  In particular, do not release the
    // reservation and re-select buckets: that would allow a policy change
    // between admission and settlement to move spend to another bucket.
    settle_persisted_allocations(
        tx,
        event_id,
        usage_log_id,
        user_id,
        project_id,
        public_model_id,
        currency,
        wallet,
        amount_micros,
        incurred_at,
        reservation_id,
    )
    .await
}

async fn settle_persisted_allocations(
    tx: &mut Transaction<'_, Postgres>,
    event_id: i64,
    usage_log_id: i64,
    user_id: i64,
    project_id: i64,
    public_model_id: i64,
    currency: &str,
    wallet: Option<LockedWallet>,
    amount_micros: i64,
    incurred_at: DateTime<Utc>,
    reservation_id: Option<i64>,
) -> Result<SettlementFunding, String> {
    let settled_at = Utc::now();
    let allocations = sqlx::query(
        "SELECT a.id,a.source_type,a.source_id,a.reserved_micros,a.captured_micros,a.released_micros \
         FROM project_wallet_reservation_allocations a \
         JOIN project_wallet_reservations r ON r.id=a.reservation_id \
         WHERE r.id=$1 \
         ORDER BY CASE WHEN a.allocation_class='DEDICATED' THEN 0 ELSE 1 END, \
                  a.expires_at_snapshot NULLS LAST,a.id FOR UPDATE",
    )
    .bind(reservation_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| e.to_string())?;
    let mut remaining = amount_micros.max(0);
    let mut subscription_amount = 0_i64;
    let mut credit_amount = 0_i64;
    for a in allocations {
        let id: i64 = a.get("id");
        let source: String = a.get("source_type");
        let reserved: i64 = a.get("reserved_micros");
        let captured: i64 = a.get("captured_micros");
        let released: i64 = a.get("released_micros");
        let outstanding = reserved
            .saturating_sub(captured)
            .saturating_sub(released)
            .max(0);
        let take = remaining.min(outstanding);
        let release = outstanding.saturating_sub(take);
        if source == "subscription_bucket" {
            let bucket_id: i64 = a.get("source_id");
            sqlx::query("UPDATE subscription_allowance_buckets SET \
                reserved_micros=GREATEST(reserved_micros-$1,0), \
                consumed_micros=consumed_micros+$2, \
                status=CASE WHEN status='draining' AND GREATEST(reserved_micros-$1,0)=0 THEN 'expired' ELSE status END, \
                updated_at=$3 WHERE id=$4")
                .bind(outstanding).bind(take).bind(settled_at).bind(bucket_id)
                .execute(&mut **tx).await.map_err(|e| e.to_string())?;
            subscription_amount += take;
        } else {
            credit_amount += take;
        }
        sqlx::query("UPDATE project_wallet_reservation_allocations SET captured_micros=captured_micros+$1,released_micros=released_micros+$2 WHERE id=$3")
            .bind(take).bind(release).bind(id).execute(&mut **tx).await.map_err(|e| e.to_string())?;
        remaining -= take;
    }
    // An actual charge can exceed the estimate.  Append allocations from
    // currently available buckets in the same dedicated/general order.
    if remaining > 0 {
        let extra = sqlx::query("SELECT b.id,b.quota_class,b.scope_snapshot,b.expires_at,GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0) available FROM subscription_allowance_buckets b JOIN user_subscriptions s ON s.id=b.subscription_id JOIN subscription_plans p ON p.id=s.plan_id JOIN subscription_entitlement_snapshots es ON es.id=b.entitlement_snapshot_id JOIN user_subscription_projects usp ON usp.subscription_id=s.id AND usp.project_id=$1 WHERE s.user_id=$2 AND s.status IN ('active','cancel_pending') AND b.status='active' AND b.period_start<=$3 AND b.expires_at>$3 AND p.currency=$4 AND (b.quota_class='GENERAL' OR EXISTS(SELECT 1 FROM subscription_entitlement_snapshot_items esi WHERE esi.snapshot_id=es.id AND esi.quota_rule_snapshot_id=b.quota_rule_snapshot_id AND esi.public_model_id=$5)) ORDER BY CASE WHEN b.quota_class='DEDICATED' THEN 0 ELSE 1 END,b.expires_at,b.id FOR UPDATE OF b")
            .bind(project_id).bind(user_id).bind(incurred_at).bind(currency).bind(public_model_id)
            .fetch_all(&mut **tx).await.map_err(|e| e.to_string())?;
        for b in extra {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(b.get::<i64, _>("available").max(0));
            if take == 0 {
                continue;
            }
            let bucket_id: i64 = b.get("id");
            if let Some(reservation_id) = reservation_id {
                let class: String = b.get("quota_class");
                let scope: Value = b.get("scope_snapshot");
                let expiry: DateTime<Utc> = b.get("expires_at");
                sqlx::query("INSERT INTO project_wallet_reservation_allocations(reservation_id,source_type,source_id,amount_micros,reserved_micros,captured_micros,allocation_class,scope_snapshot,expires_at_snapshot,created_at) VALUES($1,'subscription_bucket',$2,$3,$3,$3,$4,$5,$6,$7) ON CONFLICT(reservation_id,source_type,source_id) DO UPDATE SET amount_micros=project_wallet_reservation_allocations.amount_micros+EXCLUDED.amount_micros,reserved_micros=project_wallet_reservation_allocations.reserved_micros+EXCLUDED.reserved_micros,captured_micros=project_wallet_reservation_allocations.captured_micros+EXCLUDED.captured_micros")
                    .bind(reservation_id).bind(bucket_id).bind(take).bind(class).bind(sqlx::types::Json(scope)).bind(expiry).bind(settled_at)
                    .execute(&mut **tx).await.map_err(|e| e.to_string())?;
            }
            sqlx::query("UPDATE subscription_allowance_buckets SET consumed_micros=consumed_micros+$1,updated_at=$2 WHERE id=$3")
                .bind(take).bind(settled_at).bind(bucket_id).execute(&mut **tx).await.map_err(|e| e.to_string())?;
            subscription_amount += take;
            remaining -= take;
        }
    }
    if remaining > 0 {
        let available = wallet.map(LockedWallet::available_credit).unwrap_or(0);
        let take = remaining.min(available);
        if take > 0
            && let (Some(reservation_id), Some(wallet)) = (reservation_id, wallet)
        {
            sqlx::query("INSERT INTO project_wallet_reservation_allocations(reservation_id,source_type,source_id,amount_micros,reserved_micros,captured_micros,allocation_class,scope_snapshot,created_at) VALUES($1,'project_credit',$2,$3,$3,$3,'PROJECT_CREDIT','{}'::jsonb,$4) ON CONFLICT(reservation_id,source_type,source_id) DO UPDATE SET amount_micros=project_wallet_reservation_allocations.amount_micros+EXCLUDED.amount_micros,reserved_micros=project_wallet_reservation_allocations.reserved_micros+EXCLUDED.reserved_micros,captured_micros=project_wallet_reservation_allocations.captured_micros+EXCLUDED.captured_micros")
                .bind(reservation_id).bind(wallet.id).bind(take).bind(settled_at)
                .execute(&mut **tx).await.map_err(|e| e.to_string())?;
        }
        credit_amount += take;
        remaining -= take;
    }
    if credit_amount > 0 {
        let wallet_id = wallet
            .map(|w| w.id)
            .ok_or_else(|| "project wallet disappeared".to_string())?;
        sqlx::query("INSERT INTO project_credit_ledger_entries(wallet_id,amount_micros,entry_type,reference_type,reference_id,idempotency_key,description,metadata,created_at) VALUES($1,$2,'usage_charge','usage_log',$3,$4,'Usage settlement',$5,$6)")
            .bind(wallet_id).bind(-credit_amount).bind(usage_log_id.to_string())
            .bind(format!("usage-charge:{usage_log_id}"))
            .bind(sqlx::types::Json(json!({"charge_event_id":event_id}))).bind(settled_at)
            .execute(&mut **tx).await.map_err(|e| e.to_string())?;
    }
    let shortfall = remaining;
    let funded = amount_micros.saturating_sub(shortfall);
    let status = if shortfall == 0 {
        "settled"
    } else {
        "insufficient_funds"
    };
    sqlx::query("INSERT INTO charge_settlements(charge_event_id,user_id,wallet_id,amount_micros,subscription_amount_micros,credit_amount_micros,status,detail_snapshot,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(event_id).bind(user_id).bind(wallet.map(|w| w.id)).bind(amount_micros).bind(subscription_amount).bind(credit_amount)
        .bind(status).bind(sqlx::types::Json(json!({"funded_micros":funded,"shortfall_micros":shortfall,"funding_order":["dedicated","general","project_credit"]}))).bind(settled_at)
        .execute(&mut **tx).await.map_err(|e| e.to_string())?;
    sqlx::query("UPDATE customer_charge_events SET status=$1 WHERE id=$2")
        .bind(status)
        .bind(event_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
    Ok(SettlementFunding {
        funded_micros: funded,
        shortfall_micros: shortfall,
    })
}

async fn lock_project_wallet(
    tx: &mut Transaction<'_, Postgres>,
    project_id: i64,
    currency: &str,
) -> Result<Option<LockedWallet>, String> {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT id,credit_balance_micros,credit_reserved_micros FROM project_wallets \
         WHERE project_id=$1 AND currency=$2 AND status='active' ORDER BY id LIMIT 1 FOR UPDATE",
    )
    .bind(project_id)
    .bind(currency)
    .fetch_optional(&mut **tx)
    .await
    .map(|row| {
        row.map(
            |(id, credit_balance_micros, credit_reserved_micros)| LockedWallet {
                id,
                credit_balance_micros,
                credit_reserved_micros,
            },
        )
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::money::{DEFAULT_ACCOUNTING_CURRENCY_CODE, STATION_CREDIT_CODE};
    use conduit_llm::TokenDetails;

    async fn insert_fixture_user(
        pool: &sqlx::PgPool,
        email_label: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>("INSERT INTO users(email,password) VALUES($1,$2) RETURNING id")
            .bind(format!("{email_label}@example.test"))
            .bind("fixture-password-hash")
            .fetch_one(pool)
            .await
    }

    #[tokio::test]
    async fn wallet_admission_gate_serializes_only_the_same_wallet() {
        let gates = Arc::new(WalletAdmissionGates::default());
        let first = gates.acquire(7, STATION_CREDIT_CODE).await;

        tokio::time::timeout(
            Duration::from_millis(100),
            gates.acquire(8, STATION_CREDIT_CODE),
        )
        .await
        .expect("different wallet must acquire independently");

        let waiter_gates = gates.clone();
        let waiter =
            tokio::spawn(async move { waiter_gates.acquire(7, STATION_CREDIT_CODE).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "same wallet must remain queued");

        drop(first);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("same-wallet waiter must resume after release")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn postgres_wallet_snapshots_follow_ledger_and_reservation_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,status,description,profiles) \
             VALUES($1,'active','','{}'::jsonb) RETURNING id",
        )
        .bind(format!("snapshot-project-{suffix}"))
        .fetch_one(&isolated.pool)
        .await?;
        let wallet_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
             VALUES($1,'STATION_CREDIT','active',now(),now()) RETURNING id",
        )
        .bind(project_id)
        .fetch_one(&isolated.pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_credit_ledger_entries \
             (wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
             VALUES($1,1000000,'admin_grant',$2,'{}',now())",
        )
        .bind(wallet_id)
        .bind(format!("snapshot-grant-{suffix}"))
        .execute(&isolated.pool)
        .await?;
        let reservation_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_wallet_reservations \
             (wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) \
             VALUES($1,0,0,$2,400000,'reserved',now()+interval '1 hour',now(),now()) RETURNING id",
        )
        .bind(wallet_id)
        .bind(format!("snapshot-request-{suffix}"))
        .fetch_one(&isolated.pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_wallet_reservation_allocations \
             (reservation_id,source_type,source_id,amount_micros,reserved_micros,allocation_class,created_at) \
             VALUES($1,'project_credit',$2,400000,400000,'PROJECT_CREDIT',now())",
        )
        .bind(reservation_id)
        .bind(wallet_id)
        .execute(&isolated.pool)
        .await?;
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT credit_balance_micros,credit_reserved_micros FROM project_wallets WHERE id=$1"
            )
            .bind(wallet_id)
            .fetch_one(&isolated.pool)
            .await?,
            (1_000_000, 400_000)
        );

        sqlx::query("UPDATE project_wallet_reservations SET status='released' WHERE id=$1")
            .bind(reservation_id)
            .execute(&isolated.pool)
            .await?;
        sqlx::query(
            "INSERT INTO project_credit_ledger_entries \
             (wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
             VALUES($1,-250000,'usage_charge',$2,'{}',now())",
        )
        .bind(wallet_id)
        .bind(format!("snapshot-charge-{suffix}"))
        .execute(&isolated.pool)
        .await?;
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT credit_balance_micros,credit_reserved_micros FROM project_wallets WHERE id=$1"
            )
            .bind(wallet_id)
            .fetch_one(&isolated.pool)
            .await?,
            (750_000, 0)
        );
        isolated.cleanup().await?;
        Ok(())
    }

    struct FundsFixture {
        isolated: crate::postgres_test_support::IsolatedPostgres,
        user_id: i64,
        project_id: i64,
        model_id: i64,
        wallet_id: i64,
        bucket_id: i64,
        grant_id: i64,
        incurred_at: DateTime<Utc>,
    }

    impl FundsFixture {
        async fn new(
            dsn: &str,
            subscription_micros: i64,
            subscription_reserved_micros: i64,
            wallet_micros: i64,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            let isolated = crate::postgres_test_support::IsolatedPostgres::new(dsn).await?;
            let pool = &isolated.pool;
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let user_id = insert_fixture_user(pool, &format!("funds-{suffix}")).await?;
            let incurred_at = Utc::now() - chrono::Duration::hours(1);
            let period_start = incurred_at - chrono::Duration::days(1);
            let period_end = incurred_at + chrono::Duration::days(30);
            let project_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO projects(name,status,description,profiles) \
                 VALUES($1,'active','','{}'::jsonb) RETURNING id",
            )
            .bind(format!("funds-project-{suffix}"))
            .fetch_one(pool)
            .await?;
            let model_key = format!("funds-model-{suffix}");
            let model_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) \
                 VALUES('test',$1,$1,'','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id",
            )
            .bind(&model_key)
            .fetch_one(pool)
            .await?;
            let wallet_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
                 VALUES($1,$2,'active',now(),now()) RETURNING id",
            )
            .bind(project_id)
            .bind(STATION_CREDIT_CODE)
            .fetch_one(pool)
            .await?;
            sqlx::query(
                "INSERT INTO project_credit_ledger_entries \
                 (wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
                 VALUES($1,$2,'admin_grant',$3,'{}',now())",
            )
            .bind(wallet_id)
            .bind(wallet_micros)
            .bind(format!("funds-grant-{suffix}"))
            .execute(pool)
            .await?;
            let plan_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_plans \
                  (name,currency,interval_unit,status,created_at,updated_at) \
                  VALUES($1,$2,'month','enabled',now(),now()) RETURNING id",
            )
            .bind(format!("funds-plan-{suffix}"))
            .bind(STATION_CREDIT_CODE)
            .fetch_one(pool)
            .await?;
            let quota_rule_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_quota_rules \
                 (subscription_plan_id,rule_key,name,quota_class,amount_micros,rollover_mode,created_at,updated_at) \
                 VALUES($1,'general','General','GENERAL',$2,'none',now(),now()) RETURNING id",
            )
            .bind(plan_id)
            .bind(subscription_micros)
            .fetch_one(pool)
            .await?;
            let assignment_key = format!("funds-assignment-{suffix}");
            let assignment_request_snapshot = json!({
                "fixture": "usage_charge_settler_postgres::FundsFixture",
                "user_id": user_id,
                "plan_id": plan_id,
                "project_id": project_id,
            });
            let subscription_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO user_subscriptions \
                 (user_id,plan_id,assignment_key,assignment_request_snapshot,status, \
                  current_period_start,current_period_end,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,'active',$5,$6,now(),now()) RETURNING id",
            )
            .bind(user_id)
            .bind(plan_id)
            .bind(assignment_key)
            .bind(sqlx::types::Json(assignment_request_snapshot))
            .bind(period_start)
            .bind(period_end)
            .fetch_one(pool)
            .await?;
            sqlx::query(
                "INSERT INTO user_subscription_projects(subscription_id,project_id,created_at) \
                 VALUES($1,$2,now())",
            )
            .bind(subscription_id)
            .bind(project_id)
            .execute(pool)
            .await?;
            let quota_rule_snapshot_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO user_subscription_quota_rule_snapshots \
                 (subscription_id,rule_key,rule_name,quota_class,amount_micros,rollover_mode,access_plan_versions,created_at) \
                 VALUES($1,'general','General','GENERAL',$2,'none','[]'::jsonb,now()) RETURNING id",
            )
            .bind(subscription_id)
            .bind(subscription_micros)
            .fetch_one(pool)
            .await?;
            let entitlement_snapshot_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_entitlement_snapshots \
                 (subscription_id,period_start,period_end,created_at) \
                 VALUES($1,$2,$3,now()) RETURNING id",
            )
            .bind(subscription_id)
            .bind(period_start)
            .bind(period_end)
            .fetch_one(pool)
            .await?;
            let access_plan_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO access_plans(name,status,created_at,updated_at) \
                 VALUES($1,'enabled',now(),now()) RETURNING id",
            )
            .bind(format!("funds-access-{suffix}"))
            .fetch_one(pool)
            .await?;
            let access_version_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO access_plan_versions \
                 (access_plan_id,version,status,reference_id,created_at,updated_at) \
                 VALUES($1,1,'published',$2,now(),now()) RETURNING id",
            )
            .bind(access_plan_id)
            .bind(format!("funds-access-v1-{suffix}"))
            .fetch_one(pool)
            .await?;
            sqlx::query(
                "INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) \
                 VALUES($1,$2,now())",
            )
            .bind(access_version_id)
            .bind(model_id)
            .execute(pool)
            .await?;
            let grant_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO project_access_grants \
                 (project_id,access_plan_version_id,source_type,source_id,status,valid_from,valid_until,created_at,updated_at) \
                 VALUES($1,$2,'subscription',$3,'active',$4,$5,now(),now()) RETURNING id",
            )
            .bind(project_id)
            .bind(access_version_id)
            .bind(format!("{subscription_id}:{access_plan_id}"))
            .bind(period_start)
            .bind(period_end)
            .fetch_one(pool)
            .await?;
            let bucket_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_allowance_buckets \
                 (subscription_id,quota_rule_id,quota_rule_snapshot_id,entitlement_snapshot_id, \
                  quota_class,scope_snapshot,issued_at,period_start,period_end,expires_at, \
                  granted_micros,consumed_micros,reserved_micros,status,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,'GENERAL','{}'::jsonb,now(),$5,$6,$6,$7,0,$8, \
                        'active',now(),now()) RETURNING id",
            )
            .bind(subscription_id)
            .bind(quota_rule_id)
            .bind(quota_rule_snapshot_id)
            .bind(entitlement_snapshot_id)
            .bind(period_start)
            .bind(period_end)
            .bind(subscription_micros)
            .bind(subscription_reserved_micros)
            .fetch_one(pool)
            .await?;
            Ok(Self {
                isolated,
                user_id,
                project_id,
                model_id,
                wallet_id,
                bucket_id,
                grant_id,
                incurred_at,
            })
        }

        async fn charge_event(&self, usage_log_id: i64) -> Result<i64, Box<dyn std::error::Error>> {
            Ok(sqlx::query_scalar::<_, i64>(
                "INSERT INTO customer_charge_events \
                 (usage_log_id,request_id,public_model_id,amount,currency,applied_rules_snapshot, \
                   usage_snapshot,calculation_snapshot,status,created_at) \
                  VALUES($1,$1,$2,0,$3,'{}'::jsonb,'{}'::jsonb,'{}'::jsonb,'calculated',now()) \
                  RETURNING id",
            )
            .bind(usage_log_id)
            .bind(self.model_id)
            .bind(STATION_CREDIT_CODE)
            .fetch_one(&self.isolated.pool)
            .await?)
        }
    }

    async fn settle_fixture(
        fixture: &FundsFixture,
        event_id: i64,
        usage_log_id: i64,
        amount_micros: i64,
        incurred_at: DateTime<Utc>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut tx = fixture.isolated.pool.begin().await?;
        settle_funds(
            &mut tx,
            event_id,
            usage_log_id,
            fixture.user_id,
            fixture.project_id,
            fixture.model_id,
            STATION_CREDIT_CODE,
            amount_micros,
            incurred_at,
        )
        .await
        .map_err(std::io::Error::other)?;
        tx.commit().await?;
        Ok(())
    }

    /// Fresh PostgreSQL fixture for reservation allocation/capture tests.
    ///
    /// Each instance owns an isolated schema, so these tests exercise the real
    /// migration constraints and triggers without inheriting state from another
    /// test or from a developer database.
    struct ReservationFixture {
        isolated: crate::postgres_test_support::IsolatedPostgres,
        user_id: i64,
        project_id: i64,
        target_model_id: i64,
        other_model_id: i64,
        target_model_key: String,
        wallet_id: i64,
        api_key_id: i64,
        plan_id: i64,
        subscription_id: i64,
        entitlement_snapshot_id: i64,
        period_start: DateTime<Utc>,
    }

    impl ReservationFixture {
        async fn new(dsn: &str, wallet_micros: i64) -> Result<Self, Box<dyn std::error::Error>> {
            let isolated = crate::postgres_test_support::IsolatedPostgres::new(dsn).await?;
            let pool = &isolated.pool;
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let user_id = insert_fixture_user(pool, &format!("reservation-{suffix}")).await?;
            let period_start = Utc::now() - chrono::Duration::days(1);
            let period_end = Utc::now() + chrono::Duration::days(90);
            let project_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO projects(name,status,description,profiles) \
                 VALUES($1,'active','','{}'::jsonb) RETURNING id",
            )
            .bind(format!("reservation-project-{suffix}"))
            .fetch_one(pool)
            .await?;
            let target_model_key = format!("reservation-target-{suffix}");
            let target_model_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) \
                 VALUES('test',$1,$1,'','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id",
            )
            .bind(&target_model_key)
            .fetch_one(pool)
            .await?;
            let other_model_key = format!("reservation-other-{suffix}");
            let other_model_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) \
                 VALUES('test',$1,$1,'','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id",
            )
            .bind(&other_model_key)
            .fetch_one(pool)
            .await?;
            let wallet_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
                 VALUES($1,$2,'active',now(),now()) RETURNING id",
            )
            .bind(project_id)
            .bind(STATION_CREDIT_CODE)
            .fetch_one(pool)
            .await?;
            if wallet_micros > 0 {
                sqlx::query(
                    "INSERT INTO project_credit_ledger_entries \
                     (wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
                     VALUES($1,$2,'admin_grant',$3,'{}',now())",
                )
                .bind(wallet_id)
                .bind(wallet_micros)
                .bind(format!("reservation-grant-{suffix}"))
                .execute(pool)
                .await?;
            }
            let plan_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_plans \
                 (name,currency,interval_unit,status,created_at,updated_at) \
                 VALUES($1,$2,'month','enabled',now(),now()) RETURNING id",
            )
            .bind(format!("reservation-plan-{suffix}"))
            .bind(STATION_CREDIT_CODE)
            .fetch_one(pool)
            .await?;
            let assignment_key = format!("reservation-assignment-{suffix}");
            let assignment_request_snapshot = json!({
                "fixture": "usage_charge_settler_postgres::ReservationFixture",
                "user_id": user_id,
                "plan_id": plan_id,
                "project_id": project_id,
            });
            let subscription_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO user_subscriptions \
                 (user_id,plan_id,assignment_key,assignment_request_snapshot,status, \
                  current_period_start,current_period_end,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,'active',$5,$6,now(),now()) RETURNING id",
            )
            .bind(user_id)
            .bind(plan_id)
            .bind(assignment_key)
            .bind(sqlx::types::Json(assignment_request_snapshot))
            .bind(period_start)
            .bind(period_end)
            .fetch_one(pool)
            .await?;
            sqlx::query(
                "INSERT INTO user_subscription_projects(subscription_id,project_id,created_at) \
                 VALUES($1,$2,now())",
            )
            .bind(subscription_id)
            .bind(project_id)
            .execute(pool)
            .await?;
            let entitlement_snapshot_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_entitlement_snapshots \
                 (subscription_id,period_start,period_end,created_at) \
                 VALUES($1,$2,$3,now()) RETURNING id",
            )
            .bind(subscription_id)
            .bind(period_start)
            .bind(period_end)
            .fetch_one(pool)
            .await?;
            let api_key_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO api_keys(user_id,project_id,key,name,\"type\",status,scopes,profiles) \
                 VALUES($1,$2,$3,$4,'user','enabled','[]'::jsonb,'{}'::jsonb) RETURNING id",
            )
            .bind(user_id)
            .bind(project_id)
            .bind(format!("reservation-key-{suffix}"))
            .bind(format!("Reservation Key {suffix}"))
            .fetch_one(pool)
            .await?;
            let book_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO price_books(name,currency,status,is_default) \
                 VALUES($1,$2,'enabled',true) RETURNING id",
            )
            .bind(format!("reservation-book-{suffix}"))
            .bind(DEFAULT_ACCOUNTING_CURRENCY_CODE)
            .fetch_one(pool)
            .await?;
            let version_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO price_book_versions(price_book_id,version,status,reference_id) \
                 VALUES($1,1,'published',$2) RETURNING id",
            )
            .bind(book_id)
            .bind(format!("reservation-price-{suffix}"))
            .fetch_one(pool)
            .await?;
            let price = json!({
                "items": [{
                    "itemCode": "prompt_tokens",
                    "pricing": {"mode": "usage_per_unit", "usagePerUnit": "10"}
                }]
            });
            sqlx::query(
                "INSERT INTO price_book_items(price_book_version_id,public_model_id,price) \
                 VALUES($1,$2,$3)",
            )
            .bind(version_id)
            .bind(target_model_id)
            .bind(sqlx::types::Json(price))
            .execute(pool)
            .await?;

            Ok(Self {
                isolated,
                user_id,
                project_id,
                target_model_id,
                other_model_id,
                target_model_key,
                wallet_id,
                api_key_id,
                plan_id,
                subscription_id,
                entitlement_snapshot_id,
                period_start,
            })
        }

        async fn add_bucket(
            &self,
            rule_key: &str,
            quota_class: &str,
            amount_micros: i64,
            expires_at: DateTime<Utc>,
            entitled_model_id: Option<i64>,
        ) -> Result<i64, Box<dyn std::error::Error>> {
            let pool = &self.isolated.pool;
            let quota_rule_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_quota_rules \
                 (subscription_plan_id,rule_key,name,quota_class,amount_micros,rollover_mode,created_at,updated_at) \
                 VALUES($1,$2,$2,$3,$4,'none',now(),now()) RETURNING id",
            )
            .bind(self.plan_id)
            .bind(rule_key)
            .bind(quota_class)
            .bind(amount_micros)
            .fetch_one(pool)
            .await?;
            let access_plan_versions = if quota_class == "DEDICATED" {
                json!([{"fixtureVersion": rule_key}])
            } else {
                json!([])
            };
            let quota_rule_snapshot_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO user_subscription_quota_rule_snapshots \
                 (subscription_id,rule_key,rule_name,quota_class,amount_micros,rollover_mode,access_plan_versions,created_at) \
                 VALUES($1,$2,$2,$3,$4,'none',$5,now()) RETURNING id",
            )
            .bind(self.subscription_id)
            .bind(rule_key)
            .bind(quota_class)
            .bind(amount_micros)
            .bind(sqlx::types::Json(access_plan_versions))
            .fetch_one(pool)
            .await?;
            if let Some(model_id) = entitled_model_id {
                sqlx::query(
                    "INSERT INTO subscription_entitlement_snapshot_items \
                     (snapshot_id,quota_rule_snapshot_id,public_model_id) VALUES($1,$2,$3)",
                )
                .bind(self.entitlement_snapshot_id)
                .bind(quota_rule_snapshot_id)
                .bind(model_id)
                .execute(pool)
                .await?;
            }
            let scope_snapshot = entitled_model_id.map_or_else(
                || json!({}),
                |model_id| json!({"publicModelIds": [model_id]}),
            );
            Ok(sqlx::query_scalar::<_, i64>(
                "INSERT INTO subscription_allowance_buckets \
                 (subscription_id,quota_rule_id,quota_rule_snapshot_id,entitlement_snapshot_id, \
                  quota_class,scope_snapshot,issued_at,period_start,period_end,expires_at, \
                  granted_micros,status,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,$5,$6,now(),$7,$8,$8,$9,'active',now(),now()) \
                 RETURNING id",
            )
            .bind(self.subscription_id)
            .bind(quota_rule_id)
            .bind(quota_rule_snapshot_id)
            .bind(self.entitlement_snapshot_id)
            .bind(quota_class)
            .bind(sqlx::types::Json(scope_snapshot))
            .bind(self.period_start)
            .bind(expires_at)
            .bind(amount_micros)
            .fetch_one(pool)
            .await?)
        }

        async fn reserve_estimate(
            &self,
            request_key: &str,
        ) -> Result<i64, Box<dyn std::error::Error>> {
            let mut settler = PgUsageChargeSettler::new(self.isolated.pool.clone());
            settler.enforcement_mode = BillingEnforcementMode::HardEnforce;
            let result = settler
                .reserve_request(&BillingAdmissionInput {
                    request_key: request_key.to_owned(),
                    project_id: self.project_id.to_string(),
                    api_key_id: Some(self.api_key_id.to_string()),
                    public_model: self.target_model_key.clone(),
                    estimated_input_tokens: 1_000_000,
                    max_output_tokens: 0,
                })
                .await
                .map_err(std::io::Error::other)?;
            assert_eq!(result.as_deref(), Some(request_key));
            Ok(sqlx::query_scalar::<_, i64>(
                "SELECT id FROM project_wallet_reservations WHERE request_id=$1",
            )
            .bind(request_key)
            .fetch_one(&self.isolated.pool)
            .await?)
        }

        async fn charge_event(&self, usage_log_id: i64) -> Result<i64, Box<dyn std::error::Error>> {
            Ok(sqlx::query_scalar::<_, i64>(
                "INSERT INTO customer_charge_events \
                 (usage_log_id,request_id,public_model_id,amount,currency,applied_rules_snapshot, \
                  usage_snapshot,calculation_snapshot,status,created_at) \
                 VALUES($1,$1,$2,0,$3,'{}'::jsonb,'{}'::jsonb,'{}'::jsonb,'calculated',now()) \
                 RETURNING id",
            )
            .bind(usage_log_id)
            .bind(self.target_model_id)
            .bind(STATION_CREDIT_CODE)
            .fetch_one(&self.isolated.pool)
            .await?)
        }

        async fn capture(
            &self,
            request_key: &str,
            amount_micros: i64,
            usage_log_id: i64,
        ) -> Result<SettlementFunding, Box<dyn std::error::Error>> {
            let event_id = self.charge_event(usage_log_id).await?;
            let mut tx = self.isolated.pool.begin().await?;
            let wallet = lock_project_wallet(&mut tx, self.project_id, STATION_CREDIT_CODE)
                .await
                .map_err(std::io::Error::other)?
                .ok_or_else(|| std::io::Error::other("fixture wallet not found"))?;
            let reservation = begin_pg_reservation_capture(
                &mut tx,
                request_key,
                wallet.id,
                self.user_id,
                self.target_model_id,
                Utc::now(),
            )
            .await
            .map_err(std::io::Error::other)?
            .ok_or_else(|| std::io::Error::other("fixture reservation not found"))?;
            let funding = settle_funds_after_wallet_lock(
                &mut tx,
                event_id,
                usage_log_id,
                self.user_id,
                self.project_id,
                self.target_model_id,
                STATION_CREDIT_CODE,
                Some(wallet),
                amount_micros,
                Utc::now(),
                Some(reservation.id),
            )
            .await
            .map_err(std::io::Error::other)?;
            finish_pg_reservation_capture(
                &mut tx,
                reservation.id,
                funding.funded_micros,
                funding.shortfall_micros,
                Utc::now(),
            )
            .await
            .map_err(std::io::Error::other)?;
            tx.commit().await?;
            Ok(funding)
        }

        async fn insert_usage_log(&self) -> Result<i64, Box<dyn std::error::Error>> {
            let request_id = sqlx::query_scalar::<_, i64>(
                "INSERT INTO requests(api_key_id,project_id,model_id,request_body,status) \
                 VALUES($1,$2,$3,'{}'::jsonb,'completed') RETURNING id",
            )
            .bind(self.api_key_id)
            .bind(self.project_id)
            .bind(&self.target_model_key)
            .fetch_one(&self.isolated.pool)
            .await?;
            Ok(sqlx::query_scalar::<_, i64>(
                "INSERT INTO usage_logs \
                 (request_id,api_key_id,project_id,channel_id,model_id,prompt_tokens,completion_tokens,total_tokens, \
                  prompt_audio_tokens,prompt_cached_tokens,prompt_write_cached_tokens,prompt_write_cached_tokens_5m, \
                  prompt_write_cached_tokens_1h,completion_audio_tokens,completion_reasoning_tokens, \
                  completion_accepted_prediction_tokens,completion_rejected_prediction_tokens,\"source\",format, \
                  total_cost,cost_items,cost_price_reference_id,created_at,updated_at) \
                 VALUES($1,$2,$3,NULL,$4,1000000,0,1000000,0,0,0,0,0,0,0,0,0, \
                        'api','openai/chat_completions',NULL,'[]'::jsonb,NULL,now(),now()) RETURNING id",
            )
            .bind(request_id)
            .bind(self.api_key_id)
            .bind(self.project_id)
            .bind(&self.target_model_key)
            .fetch_one(&self.isolated.pool)
            .await?)
        }
    }

    #[tokio::test]
    async fn postgres_reservation_allocates_matching_dedicated_then_general_then_credit_in_fefo_order_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        const BILLION: i64 = 1_000_000_000;
        let fixture = ReservationFixture::new(&dsn, 40 * BILLION).await?;
        let now = Utc::now();
        let unmatched = fixture
            .add_bucket(
                "dedicated-other",
                "DEDICATED",
                99 * BILLION,
                now + chrono::Duration::days(1),
                Some(fixture.other_model_id),
            )
            .await?;
        let dedicated_early = fixture
            .add_bucket(
                "dedicated-early",
                "DEDICATED",
                10 * BILLION,
                now + chrono::Duration::days(2),
                Some(fixture.target_model_id),
            )
            .await?;
        let dedicated_late = fixture
            .add_bucket(
                "dedicated-late",
                "DEDICATED",
                15 * BILLION,
                now + chrono::Duration::days(3),
                Some(fixture.target_model_id),
            )
            .await?;
        let general_early = fixture
            .add_bucket(
                "general-early",
                "GENERAL",
                12 * BILLION,
                now + chrono::Duration::days(4),
                None,
            )
            .await?;
        let general_late = fixture
            .add_bucket(
                "general-late",
                "GENERAL",
                23 * BILLION,
                now + chrono::Duration::days(5),
                None,
            )
            .await?;
        let reservation_id = fixture.reserve_estimate("funding-order").await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT amount_micros FROM project_wallet_reservations WHERE id=$1",
            )
            .bind(reservation_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            100 * BILLION
        );
        let allocations: Vec<(String, i64, i64, String)> = sqlx::query_as(
            "SELECT source_type,source_id,reserved_micros,allocation_class \
             FROM project_wallet_reservation_allocations WHERE reservation_id=$1 ORDER BY id",
        )
        .bind(reservation_id)
        .fetch_all(&fixture.isolated.pool)
        .await?;
        assert_eq!(
            allocations,
            vec![
                (
                    "subscription_bucket".into(),
                    dedicated_early,
                    10 * BILLION,
                    "DEDICATED".into(),
                ),
                (
                    "subscription_bucket".into(),
                    dedicated_late,
                    15 * BILLION,
                    "DEDICATED".into(),
                ),
                (
                    "subscription_bucket".into(),
                    general_early,
                    12 * BILLION,
                    "GENERAL".into(),
                ),
                (
                    "subscription_bucket".into(),
                    general_late,
                    23 * BILLION,
                    "GENERAL".into(),
                ),
                (
                    "project_credit".into(),
                    fixture.wallet_id,
                    40 * BILLION,
                    "PROJECT_CREDIT".into(),
                ),
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$1",
            )
            .bind(unmatched)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            0,
            "a dedicated bucket for another model must not fund this request"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT credit_reserved_micros FROM project_wallets WHERE id=$1",
            )
            .bind(fixture.wallet_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            40 * BILLION
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_persisted_reservation_capture_releases_underestimate_remainder_and_records_overestimate_shortfall_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        const BILLION: i64 = 1_000_000_000;

        let lower = ReservationFixture::new(&dsn, 100 * BILLION).await?;
        let now = Utc::now();
        let lower_dedicated = lower
            .add_bucket(
                "lower-dedicated",
                "DEDICATED",
                30 * BILLION,
                now + chrono::Duration::days(2),
                Some(lower.target_model_id),
            )
            .await?;
        let lower_general = lower
            .add_bucket(
                "lower-general",
                "GENERAL",
                30 * BILLION,
                now + chrono::Duration::days(3),
                None,
            )
            .await?;
        let lower_reservation = lower.reserve_estimate("actual-below-estimate").await?;
        let lower_funding = lower
            .capture("actual-below-estimate", 70 * BILLION, 41_001)
            .await?;
        assert_eq!(
            (lower_funding.funded_micros, lower_funding.shortfall_micros),
            (70 * BILLION, 0)
        );
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT status,settled_amount_micros FROM project_wallet_reservations WHERE id=$1",
            )
            .bind(lower_reservation)
            .fetch_one(&lower.isolated.pool)
            .await?,
            ("captured".into(), 70 * BILLION)
        );
        let lower_allocations: Vec<(String, i64, i64, i64)> = sqlx::query_as(
            "SELECT source_type,source_id,captured_micros,released_micros \
             FROM project_wallet_reservation_allocations WHERE reservation_id=$1 ORDER BY id",
        )
        .bind(lower_reservation)
        .fetch_all(&lower.isolated.pool)
        .await?;
        assert_eq!(
            lower_allocations,
            vec![
                (
                    "subscription_bucket".into(),
                    lower_dedicated,
                    30 * BILLION,
                    0,
                ),
                ("subscription_bucket".into(), lower_general, 30 * BILLION, 0,),
                (
                    "project_credit".into(),
                    lower.wallet_id,
                    10 * BILLION,
                    30 * BILLION,
                ),
            ]
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT credit_balance_micros,credit_reserved_micros FROM project_wallets WHERE id=$1",
            )
            .bind(lower.wallet_id)
            .fetch_one(&lower.isolated.pool)
            .await?,
            (90 * BILLION, 0)
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "SELECT \
                   (SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$1), \
                   (SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$1), \
                   (SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$2), \
                   (SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$2)",
            )
            .bind(lower_dedicated)
            .bind(lower_general)
            .fetch_one(&lower.isolated.pool)
            .await?,
            (30 * BILLION, 0, 30 * BILLION, 0)
        );
        lower.isolated.cleanup().await?;

        let higher = ReservationFixture::new(&dsn, 10 * BILLION).await?;
        let now = Utc::now();
        let higher_dedicated = higher
            .add_bucket(
                "higher-dedicated",
                "DEDICATED",
                40 * BILLION,
                now + chrono::Duration::days(2),
                Some(higher.target_model_id),
            )
            .await?;
        let higher_general = higher
            .add_bucket(
                "higher-general",
                "GENERAL",
                80 * BILLION,
                now + chrono::Duration::days(3),
                None,
            )
            .await?;
        let higher_reservation = higher.reserve_estimate("actual-above-estimate").await?;
        let higher_funding = higher
            .capture("actual-above-estimate", 150 * BILLION, 41_002)
            .await?;
        assert_eq!(
            (
                higher_funding.funded_micros,
                higher_funding.shortfall_micros
            ),
            (130 * BILLION, 20 * BILLION)
        );
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT status,settled_amount_micros FROM project_wallet_reservations WHERE id=$1",
            )
            .bind(higher_reservation)
            .fetch_one(&higher.isolated.pool)
            .await?,
            ("partially_captured".into(), 130 * BILLION)
        );
        let higher_allocations: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT source_type,source_id,reserved_micros,captured_micros,released_micros \
             FROM project_wallet_reservation_allocations WHERE reservation_id=$1 ORDER BY id",
        )
        .bind(higher_reservation)
        .fetch_all(&higher.isolated.pool)
        .await?;
        assert_eq!(
            higher_allocations,
            vec![
                (
                    "subscription_bucket".into(),
                    higher_dedicated,
                    40 * BILLION,
                    40 * BILLION,
                    0,
                ),
                (
                    "subscription_bucket".into(),
                    higher_general,
                    80 * BILLION,
                    80 * BILLION,
                    0,
                ),
                (
                    "project_credit".into(),
                    higher.wallet_id,
                    10 * BILLION,
                    10 * BILLION,
                    0,
                ),
            ]
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT credit_balance_micros,credit_reserved_micros FROM project_wallets WHERE id=$1",
            )
            .bind(higher.wallet_id)
            .fetch_one(&higher.isolated.pool)
            .await?,
            (0, 0)
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, i64, i64)>(
                "SELECT \
                   (SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$1), \
                   (SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$1), \
                   (SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$2), \
                   (SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$2)",
            )
            .bind(higher_dedicated)
            .bind(higher_general)
            .fetch_one(&higher.isolated.pool)
            .await?,
            (40 * BILLION, 0, 80 * BILLION, 0)
        );
        assert_eq!(
            sqlx::query_as::<_, (String, i64, i64, i64)>(
                "SELECT status,subscription_amount_micros,credit_amount_micros, \
                        (detail_snapshot->>'shortfall_micros')::BIGINT \
                 FROM charge_settlements WHERE charge_event_id=( \
                   SELECT id FROM customer_charge_events WHERE usage_log_id=41002)",
            )
            .fetch_one(&higher.isolated.pool)
            .await?,
            (
                "insufficient_funds".into(),
                120 * BILLION,
                10 * BILLION,
                20 * BILLION,
            )
        );
        higher.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_expired_outbox_reservation_falls_back_and_completed_outbox_ignores_late_failure_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        const BILLION: i64 = 1_000_000_000;
        let fixture = ReservationFixture::new(&dsn, 0).await?;
        let bucket_id = fixture
            .add_bucket(
                "outbox-general",
                "GENERAL",
                200 * BILLION,
                Utc::now() + chrono::Duration::days(3),
                None,
            )
            .await?;
        let reservation_id = fixture.reserve_estimate("expired-outbox-key").await?;
        sqlx::query(
            "UPDATE project_wallet_reservations SET expires_at=now()-interval '1 second' WHERE id=$1",
        )
        .bind(reservation_id)
        .execute(&fixture.isolated.pool)
        .await?;
        let settler = PgUsageChargeSettler::new(fixture.isolated.pool.clone());
        assert_eq!(settler.cleanup_expired_reservations().await?, 1);
        let usage_log_id = fixture.insert_usage_log().await?;
        sqlx::query(
            "INSERT INTO usage_charge_outbox \
             (usage_log_id,reservation_key,status,available_at,created_at,updated_at) \
             VALUES($1,'expired-outbox-key','pending',now(),now(),now())",
        )
        .bind(usage_log_id)
        .execute(&fixture.isolated.pool)
        .await?;
        assert_eq!(settler.reconcile_missing(10).await?, 1);
        assert_eq!(
            sqlx::query_as::<_, (String, i64, i64)>(
                "SELECT status,attempts::BIGINT, \
                        (SELECT COUNT(*)::BIGINT FROM charge_settlements s \
                         JOIN customer_charge_events e ON e.id=s.charge_event_id \
                         WHERE e.usage_log_id=$1) \
                 FROM usage_charge_outbox WHERE usage_log_id=$1",
            )
            .bind(usage_log_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            ("completed".into(), 0, 1)
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Option<i64>)>(
                "SELECT status,settled_amount_micros FROM project_wallet_reservations WHERE id=$1",
            )
            .bind(reservation_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            ("expired".into(), None)
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT consumed_micros,reserved_micros FROM subscription_allowance_buckets WHERE id=$1",
            )
            .bind(bucket_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            (100 * BILLION, 0)
        );

        settler
            .mark_outbox_failed(usage_log_id, "late_worker_failure")
            .await?;
        assert_eq!(
            sqlx::query_as::<_, (String, i64, Option<String>)>(
                "SELECT status,attempts::BIGINT,last_error FROM usage_charge_outbox WHERE usage_log_id=$1",
            )
            .bind(usage_log_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            ("completed".into(), 0, None)
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_hard_admission_reserves_once_and_releases_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        // The retail quote is 10 CNY for one million tokens. At the canonical
        // default of 10,000 station credits per CNY, admission must reserve
        // 100,000 credits (100,000,000,000 micros).
        let fixture = FundsFixture::new(&dsn, 60_000_000_000, 0, 100_000_000_000).await?;
        let pool = &fixture.isolated.pool;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let api_key_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO api_keys(user_id,project_id,key,name,\"type\",status,scopes,profiles) \
             VALUES($1,$2,$3,$4,'user','enabled','[]'::jsonb,'{}'::jsonb) RETURNING id",
        )
        .bind(fixture.user_id)
        .bind(fixture.project_id)
        .bind(format!("reserve-key-{suffix}"))
        .bind(format!("Reserve Key {suffix}"))
        .fetch_one(pool)
        .await?;
        let book_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO price_books(name,currency,status,is_default) VALUES($1,$2,'enabled',true) RETURNING id",
        )
        .bind(format!("reserve-book-{suffix}"))
        .bind(DEFAULT_ACCOUNTING_CURRENCY_CODE)
        .fetch_one(pool)
        .await?;
        let version_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO price_book_versions(price_book_id,version,status,reference_id) VALUES($1,1,'published',$2) RETURNING id",
        )
        .bind(book_id)
        .bind(format!("reserve-price-{suffix}"))
        .fetch_one(pool)
        .await?;
        let price = json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"10"}}]});
        sqlx::query("INSERT INTO price_book_items(price_book_version_id,public_model_id,price) VALUES($1,$2,$3)")
            .bind(version_id).bind(fixture.model_id).bind(sqlx::types::Json(price)).execute(pool).await?;
        let request_key = format!("reserve-request-{suffix}");
        let input = BillingAdmissionInput {
            request_key: request_key.clone(),
            project_id: fixture.project_id.to_string(),
            api_key_id: Some(api_key_id.to_string()),
            public_model: sqlx::query_scalar::<_, String>(
                "SELECT model_id FROM models WHERE id=$1",
            )
            .bind(fixture.model_id)
            .fetch_one(pool)
            .await?,
            estimated_input_tokens: 1_000_000,
            max_output_tokens: 0,
        };
        let mut settler = PgUsageChargeSettler::new(pool.clone());
        let mut concurrent_settler = PgUsageChargeSettler::new(pool.clone());
        settler.enforcement_mode = BillingEnforcementMode::HardEnforce;
        concurrent_settler.enforcement_mode = BillingEnforcementMode::HardEnforce;
        let (first_reservation, concurrent_retry) = tokio::join!(
            settler.reserve_request(&input),
            concurrent_settler.reserve_request(&input)
        );
        assert_eq!(first_reservation?, Some(request_key.clone()));
        assert_eq!(concurrent_retry?, Some(request_key.clone()));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM project_wallet_reservations WHERE request_id=$1"
            )
            .bind(&request_key)
            .fetch_one(pool)
            .await?,
            1
        );
        assert!(
            sqlx::query_scalar::<_, i64>(
                "SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(fixture.bucket_id)
            .fetch_one(pool)
            .await?
                > 0
        );
        sqlx::query("UPDATE subscription_allowance_buckets SET status='draining' WHERE id=$1")
            .bind(fixture.bucket_id)
            .execute(pool)
            .await?;
        settler
            .release_request(&request_key, "test_failure")
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT reserved_micros FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(fixture.bucket_id)
            .fetch_one(pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(fixture.bucket_id)
            .fetch_one(pool)
            .await?,
            "expired"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM project_wallet_reservations WHERE request_id=$1"
            )
            .bind(&request_key)
            .fetch_one(pool)
            .await?,
            "released"
        );

        let capture_key = format!("capture-request-{suffix}");
        let mut capture_input = input.clone();
        capture_input.request_key = capture_key.clone();
        settler.reserve_request(&capture_input).await?;
        let mut model_ownership_tx = pool.begin().await?;
        sqlx::query("SELECT id FROM project_wallets WHERE id=$1 FOR UPDATE")
            .bind(fixture.wallet_id)
            .fetch_one(&mut *model_ownership_tx)
            .await?;
        let model_ownership_error = begin_pg_reservation_capture(
            &mut model_ownership_tx,
            &capture_key,
            fixture.wallet_id,
            fixture.user_id,
            fixture.model_id.saturating_add(1),
            Utc::now(),
        )
        .await
        .expect_err("a reservation key for another public model must be rejected");
        assert!(model_ownership_error.contains("another user or public model"));
        model_ownership_tx.rollback().await?;
        let foreign_project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,status,description,profiles) \
             VALUES($1,'active','','{}'::jsonb) RETURNING id",
        )
        .bind(format!("foreign-capture-project-{suffix}"))
        .fetch_one(pool)
        .await?;
        let foreign_wallet_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
             VALUES($1,$2,'active',now(),now()) RETURNING id",
        )
        .bind(foreign_project_id)
        .bind(STATION_CREDIT_CODE)
        .fetch_one(pool)
        .await?;
        let mut ownership_tx = pool.begin().await?;
        sqlx::query("SELECT id FROM project_wallets WHERE id=$1 FOR UPDATE")
            .bind(foreign_wallet_id)
            .fetch_one(&mut *ownership_tx)
            .await?;
        let ownership_error = begin_pg_reservation_capture(
            &mut ownership_tx,
            &capture_key,
            foreign_wallet_id,
            fixture.user_id,
            fixture.model_id,
            Utc::now(),
        )
        .await
        .expect_err("a reservation key from another wallet must be rejected");
        assert!(ownership_error.contains("another project wallet"));
        ownership_tx.rollback().await?;
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests(api_key_id,project_id,model_id,request_body,status) \
             VALUES($1,$2,$3,'{}'::jsonb,'completed') RETURNING id",
        )
        .bind(api_key_id)
        .bind(fixture.project_id)
        .bind(&capture_input.public_model)
        .fetch_one(pool)
        .await?;
        let usage_log_id = (uuid::Uuid::new_v4().as_u128() % i64::MAX as u128) as i64;
        let at = Utc::now();
        let row = UsageLogRow {
            id: usage_log_id.to_string(),
            request_id: request_id.to_string(),
            api_key_id: Some(api_key_id.to_string()),
            project_id: fixture.project_id.to_string(),
            channel_id: Some("1".into()),
            model_id: "upstream".into(),
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
            total_tokens: 1_000_000,
            prompt_audio_tokens: 0,
            prompt_cached_tokens: 0,
            prompt_write_cached_tokens: 0,
            prompt_write_cached_tokens_5m: 0,
            prompt_write_cached_tokens_1h: 0,
            completion_audio_tokens: 0,
            completion_reasoning_tokens: 0,
            completion_accepted_prediction_tokens: 0,
            completion_rejected_prediction_tokens: 0,
            source: "api".into(),
            format: "openai/chat_completions".into(),
            total_cost: None,
            cost_items: json!([]),
            cost_price_reference_id: None,
            created_at: at,
            updated_at: at,
        };
        let usage = Usage {
            prompt_tokens: 1_000_000,
            total_tokens: 1_000_000,
            prompt_details: TokenDetails::default(),
            completion_details: TokenDetails::default(),
            ..Default::default()
        };
        settler
            .settle_usage(&row, &usage, Some(&capture_key))
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM project_wallet_reservations WHERE request_id=$1"
            )
            .bind(&capture_key)
            .fetch_one(pool)
            .await?,
            "captured"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT settled_amount_micros FROM project_wallet_reservations WHERE request_id=$1"
            )
            .bind(&capture_key)
            .fetch_one(pool)
            .await?,
            100_000_000_000
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_settlement_is_idempotent_and_uses_subscription_before_credit_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let usage_log_id = (uuid::Uuid::new_v4().as_u128() % i64::MAX as u128) as i64;
        let user_id = insert_fixture_user(&pool, &format!("settle-{suffix}")).await?;
        let project_id=sqlx::query_scalar::<_,i64>("INSERT INTO projects(name,status,description,profiles) VALUES($1,'active','','{}'::jsonb) RETURNING id")
            .bind(format!("settle-project-{suffix}")).fetch_one(&pool).await?;
        let model_key = format!("settle-model-{suffix}");
        let model_id=sqlx::query_scalar::<_,i64>("INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) VALUES('test',$1,$1,'','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id")
            .bind(&model_key).fetch_one(&pool).await?;
        let key_id=sqlx::query_scalar::<_,i64>("INSERT INTO api_keys(user_id,project_id,key,name,\"type\",status,scopes,profiles) VALUES($1,$2,$3,$4,'user','enabled','[]'::jsonb,'{}'::jsonb) RETURNING id")
            .bind(user_id).bind(project_id).bind(format!("settle-key-{suffix}")).bind(format!("Settle Key {suffix}")).fetch_one(&pool).await?;
        let request_id=sqlx::query_scalar::<_,i64>("INSERT INTO requests(api_key_id,project_id,model_id,request_body,status) VALUES($1,$2,$3,'{}'::jsonb,'completed') RETURNING id")
            .bind(key_id).bind(project_id).bind(&model_key).fetch_one(&pool).await?;
        let book_id=sqlx::query_scalar::<_,i64>("INSERT INTO price_books(name,currency,status,is_default) VALUES($1,$2,'enabled',TRUE) RETURNING id")
            .bind(format!("settle-book-{suffix}")).bind(DEFAULT_ACCOUNTING_CURRENCY_CODE).fetch_one(&pool).await?;
        let version_id=sqlx::query_scalar::<_,i64>("INSERT INTO price_book_versions(price_book_id,version,status,reference_id) VALUES($1,1,'published',$2) RETURNING id")
            .bind(book_id).bind(format!("settle-retail-v1-{suffix}")).fetch_one(&pool).await?;
        let price = json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"10"}}]});
        sqlx::query("INSERT INTO price_book_items(price_book_version_id,public_model_id,price) VALUES($1,$2,$3)")
            .bind(version_id).bind(model_id).bind(sqlx::types::Json(price)).execute(&pool).await?;
        let wallet_id=sqlx::query_scalar::<_,i64>("INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) VALUES($1,$2,'active',now(),now()) RETURNING id")
            .bind(project_id).bind(STATION_CREDIT_CODE).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO project_credit_ledger_entries(wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) VALUES($1,100000000000,'admin_grant',$2,'{}',now())")
            .bind(wallet_id).bind(format!("settle-grant-{suffix}")).execute(&pool).await?;
        let plan_id=sqlx::query_scalar::<_,i64>("INSERT INTO subscription_plans(name,currency,interval_unit,status,created_at,updated_at) VALUES($1,$2,'month','enabled',now(),now()) RETURNING id")
            .bind(format!("settle-plan-{suffix}")).bind(STATION_CREDIT_CODE).fetch_one(&pool).await?;
        let quota_rule_id=sqlx::query_scalar::<_,i64>("INSERT INTO subscription_quota_rules(subscription_plan_id,rule_key,name,quota_class,amount_micros,rollover_mode,created_at,updated_at) VALUES($1,'general','General','GENERAL',60000000000,'none',now(),now()) RETURNING id")
            .bind(plan_id).fetch_one(&pool).await?;
        let assignment_key = format!("settle-assignment-{suffix}");
        let assignment_request_snapshot = json!({
            "fixture": "usage_charge_settler_postgres::idempotent_settlement",
            "user_id": user_id,
            "plan_id": plan_id,
            "project_id": project_id,
        });
        let subscription_id=sqlx::query_scalar::<_,i64>("INSERT INTO user_subscriptions(user_id,plan_id,assignment_key,assignment_request_snapshot,status,current_period_start,current_period_end,created_at,updated_at) VALUES($1,$2,$3,$4,'active',now()-interval '1 day',now()+interval '30 days',now(),now()) RETURNING id")
            .bind(user_id).bind(plan_id).bind(assignment_key).bind(sqlx::types::Json(assignment_request_snapshot)).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO user_subscription_projects(subscription_id,project_id,created_at) VALUES($1,$2,now())")
            .bind(subscription_id).bind(project_id).execute(&pool).await?;
        let quota_rule_snapshot_id=sqlx::query_scalar::<_,i64>("INSERT INTO user_subscription_quota_rule_snapshots(subscription_id,rule_key,rule_name,quota_class,amount_micros,rollover_mode,access_plan_versions,created_at) VALUES($1,'general','General','GENERAL',60000000000,'none','[]'::jsonb,now()) RETURNING id")
            .bind(subscription_id).fetch_one(&pool).await?;
        let entitlement_snapshot_id=sqlx::query_scalar::<_,i64>("INSERT INTO subscription_entitlement_snapshots(subscription_id,period_start,period_end,created_at) SELECT id,current_period_start,current_period_end,now() FROM user_subscriptions WHERE id=$1 RETURNING id")
            .bind(subscription_id).fetch_one(&pool).await?;
        let access_plan_id=sqlx::query_scalar::<_,i64>("INSERT INTO access_plans(name,status,created_at,updated_at) VALUES($1,'enabled',now(),now()) RETURNING id")
            .bind(format!("settle-access-{suffix}")).fetch_one(&pool).await?;
        let access_version_id=sqlx::query_scalar::<_,i64>("INSERT INTO access_plan_versions(access_plan_id,version,status,reference_id,created_at,updated_at) VALUES($1,1,'published',$2,now(),now()) RETURNING id")
            .bind(access_plan_id).bind(format!("settle-access-v1-{suffix}")).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES($1,$2,now())")
            .bind(access_version_id).bind(model_id).execute(&pool).await?;
        sqlx::query("INSERT INTO project_access_grants(project_id,access_plan_version_id,source_type,source_id,status,created_at,updated_at) VALUES($1,$2,'subscription',$3,'active',now(),now())")
            .bind(project_id).bind(access_version_id).bind(format!("{subscription_id}:{access_plan_id}")).execute(&pool).await?;
        let overlapping_access_plan_id=sqlx::query_scalar::<_,i64>("INSERT INTO access_plans(name,status,created_at,updated_at) VALUES($1,'enabled',now(),now()) RETURNING id")
            .bind(format!("settle-overlap-access-{suffix}")).fetch_one(&pool).await?;
        let overlapping_version_id=sqlx::query_scalar::<_,i64>("INSERT INTO access_plan_versions(access_plan_id,version,status,reference_id,created_at,updated_at) VALUES($1,1,'published',$2,now(),now()) RETURNING id")
            .bind(overlapping_access_plan_id).bind(format!("settle-overlap-v1-{suffix}")).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES($1,$2,now())")
            .bind(overlapping_version_id).bind(model_id).execute(&pool).await?;
        sqlx::query("INSERT INTO project_access_grants(project_id,access_plan_version_id,source_type,source_id,status,created_at,updated_at) VALUES($1,$2,'subscription',$3,'active',now(),now())")
            .bind(project_id).bind(overlapping_version_id).bind(format!("{subscription_id}:{overlapping_access_plan_id}")).execute(&pool).await?;
        let bucket_id=sqlx::query_scalar::<_,i64>("INSERT INTO subscription_allowance_buckets(subscription_id,quota_rule_id,quota_rule_snapshot_id,entitlement_snapshot_id,quota_class,scope_snapshot,issued_at,period_start,period_end,expires_at,granted_micros,status,created_at,updated_at) SELECT $1,$2,$3,$4,'GENERAL','{}'::jsonb,now(),current_period_start,current_period_end,current_period_end,60000000000,'active',now(),now() FROM user_subscriptions WHERE id=$1 RETURNING id")
            .bind(subscription_id).bind(quota_rule_id).bind(quota_rule_snapshot_id).bind(entitlement_snapshot_id).fetch_one(&pool).await?;
        let at = Utc::now();
        let row = UsageLogRow {
            id: usage_log_id.to_string(),
            request_id: request_id.to_string(),
            api_key_id: Some(key_id.to_string()),
            project_id: project_id.to_string(),
            channel_id: Some("1".into()),
            model_id: "upstream".into(),
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
            total_tokens: 1_000_000,
            prompt_audio_tokens: 0,
            prompt_cached_tokens: 0,
            prompt_write_cached_tokens: 0,
            prompt_write_cached_tokens_5m: 0,
            prompt_write_cached_tokens_1h: 0,
            completion_audio_tokens: 0,
            completion_reasoning_tokens: 0,
            completion_accepted_prediction_tokens: 0,
            completion_rejected_prediction_tokens: 0,
            source: "api".into(),
            format: "openai/chat_completions".into(),
            total_cost: Some(1.0),
            cost_items: json!([]),
            cost_price_reference_id: None,
            created_at: at,
            updated_at: at,
        };
        let usage = Usage {
            prompt_tokens: 1_000_000,
            total_tokens: 1_000_000,
            prompt_details: TokenDetails::default(),
            completion_details: TokenDetails::default(),
            ..Default::default()
        };
        let settler = PgUsageChargeSettler::new(pool.clone());
        settler.settle_usage(&row, &usage, None).await?;
        settler.settle_usage(&row, &usage, None).await?;
        let (amount, subscription, credit, status): (i64, i64, i64, String) = sqlx::query_as(
            "SELECT s.amount_micros,s.subscription_amount_micros,s.credit_amount_micros,s.status \
             FROM charge_settlements s JOIN customer_charge_events e ON e.id=s.charge_event_id \
             WHERE e.usage_log_id=$1",
        )
        .bind(usage_log_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            (amount, subscription, credit, status),
            (
                100_000_000_000,
                60_000_000_000,
                40_000_000_000,
                "settled".into()
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(bucket_id)
            .fetch_one(&pool)
            .await?,
            60_000_000_000
        );
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COALESCE(SUM(amount_micros),0)::BIGINT FROM project_credit_ledger_entries WHERE wallet_id=$1").bind(wallet_id).fetch_one(&pool).await?,60_000_000_000);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM customer_charge_events WHERE usage_log_id=$1"
            )
            .bind(usage_log_id)
            .fetch_one(&pool)
            .await?,
            1
        );
        isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_insufficient_funds_records_partial_debit_and_shortfall_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = FundsFixture::new(&dsn, 3_000_000, 0, 2_000_000).await?;
        let usage_log_id = 101;
        let event_id = fixture.charge_event(usage_log_id).await?;
        settle_fixture(
            &fixture,
            event_id,
            usage_log_id,
            10_000_000,
            fixture.incurred_at,
        )
        .await?;

        let settlement: (String, i64, i64, i64) = sqlx::query_as(
            "SELECT status,subscription_amount_micros,credit_amount_micros, \
                    (detail_snapshot->>'shortfall_micros')::BIGINT \
             FROM charge_settlements WHERE charge_event_id=$1",
        )
        .bind(event_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        assert_eq!(
            settlement,
            ("insufficient_funds".into(), 3_000_000, 2_000_000, 5_000_000)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(fixture.bucket_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            3_000_000
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1"
            )
            .bind(fixture.wallet_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            0
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_subscription_reservation_is_not_spent_and_remainder_uses_wallet_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = FundsFixture::new(&dsn, 8_000_000, 3_000_000, 5_000_000).await?;
        let usage_log_id = 102;
        let event_id = fixture.charge_event(usage_log_id).await?;
        settle_fixture(
            &fixture,
            event_id,
            usage_log_id,
            7_000_000,
            fixture.incurred_at,
        )
        .await?;

        let settlement: (String, i64, i64) = sqlx::query_as(
            "SELECT status,subscription_amount_micros,credit_amount_micros \
             FROM charge_settlements WHERE charge_event_id=$1",
        )
        .bind(event_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        assert_eq!(settlement, ("settled".into(), 5_000_000, 2_000_000));
        let bucket: (i64, i64) = sqlx::query_as(
            "SELECT consumed_micros,reserved_micros FROM subscription_allowance_buckets WHERE id=$1",
        )
        .bind(fixture.bucket_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        assert_eq!(bucket, (5_000_000, 3_000_000));
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_project_wallet_reservation_prevents_unrelated_spend_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = FundsFixture::new(&dsn, 3_000_000, 0, 5_000_000).await?;
        let reservation_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_wallet_reservations \
             (wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) \
             VALUES($1,$2,$3,$4,4000000,'reserved',now()+interval '1 hour',now(),now()) RETURNING id",
        )
        .bind(fixture.wallet_id)
        .bind(fixture.user_id)
        .bind(fixture.model_id)
        .bind("reserved-request")
        .fetch_one(&fixture.isolated.pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_wallet_reservation_allocations \
             (reservation_id,source_type,source_id,amount_micros,reserved_micros,allocation_class,created_at) \
             VALUES($1,'project_credit',$2,4000000,4000000,'PROJECT_CREDIT',now())",
        )
        .bind(reservation_id)
        .bind(fixture.wallet_id)
        .execute(&fixture.isolated.pool)
        .await?;
        let usage_log_id = 103;
        let event_id = fixture.charge_event(usage_log_id).await?;
        settle_fixture(
            &fixture,
            event_id,
            usage_log_id,
            5_000_000,
            fixture.incurred_at,
        )
        .await?;

        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM charge_settlements WHERE charge_event_id=$1"
            )
            .bind(event_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            "insufficient_funds"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1"
            )
            .bind(fixture.wallet_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            4_000_000
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_settlement_uses_usage_time_for_subscription_window_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = FundsFixture::new(&dsn, 6_000_000, 0, 10_000_000).await?;
        let incurred_at = Utc::now() - chrono::Duration::days(10);
        let start = incurred_at - chrono::Duration::days(1);
        let end = incurred_at + chrono::Duration::days(1);
        sqlx::query(
            "UPDATE subscription_allowance_buckets \
             SET period_start=$1,period_end=$2,expires_at=$2 WHERE id=$3",
        )
        .bind(start)
        .bind(end)
        .bind(fixture.bucket_id)
        .execute(&fixture.isolated.pool)
        .await?;
        sqlx::query("UPDATE project_access_grants SET valid_from=$1,valid_until=$2 WHERE id=$3")
            .bind(start)
            .bind(end)
            .bind(fixture.grant_id)
            .execute(&fixture.isolated.pool)
            .await?;
        let usage_log_id = 104;
        let event_id = fixture.charge_event(usage_log_id).await?;
        settle_fixture(&fixture, event_id, usage_log_id, 5_000_000, incurred_at).await?;

        let settlement: (i64, i64) = sqlx::query_as(
            "SELECT subscription_amount_micros,credit_amount_micros \
             FROM charge_settlements WHERE charge_event_id=$1",
        )
        .bind(event_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        assert_eq!(settlement, (5_000_000, 0));
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_concurrent_settlements_never_overdraw_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = FundsFixture::new(&dsn, 6_000_000, 0, 4_000_000).await?;
        let first_usage = 105;
        let second_usage = 106;
        let first_event = fixture.charge_event(first_usage).await?;
        let second_event = fixture.charge_event(second_usage).await?;
        let (first, second) = tokio::join!(
            settle_fixture(
                &fixture,
                first_event,
                first_usage,
                7_000_000,
                fixture.incurred_at
            ),
            settle_fixture(
                &fixture,
                second_event,
                second_usage,
                7_000_000,
                fixture.incurred_at
            )
        );
        first?;
        second?;

        let statuses: Vec<String> = sqlx::query_scalar(
            "SELECT status FROM charge_settlements WHERE charge_event_id=ANY($1) ORDER BY status",
        )
        .bind(vec![first_event, second_event])
        .fetch_all(&fixture.isolated.pool)
        .await?;
        assert_eq!(statuses, vec!["insufficient_funds", "settled"]);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT consumed_micros FROM subscription_allowance_buckets WHERE id=$1"
            )
            .bind(fixture.bucket_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            6_000_000
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1"
            )
            .bind(fixture.wallet_id)
            .fetch_one(&fixture.isolated.pool)
            .await?,
            0
        );
        fixture.isolated.cleanup().await?;
        Ok(())
    }

    #[derive(Debug, Default)]
    struct LockWaitSamples {
        samples: u64,
        samples_with_waiters: u64,
        max_lock_waiters: i64,
        max_matching_sessions: i64,
        unavailable: Option<String>,
    }

    impl LockWaitSamples {
        fn waiter_sample_ratio(&self) -> f64 {
            if self.samples == 0 {
                0.0
            } else {
                self.samples_with_waiters as f64 / self.samples as f64
            }
        }
    }

    async fn sample_benchmark_lock_waits(
        pool: sqlx::PgPool,
        application_name: String,
        sample_interval: std::time::Duration,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> LockWaitSamples {
        let mut report = LockWaitSamples::default();
        let mut interval = tokio::time::interval(sample_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return report;
                    }
                }
                _ = interval.tick() => {
                    let sample = sqlx::query_as::<_, (i64, i64)>(
                        "SELECT COUNT(*) FILTER (WHERE wait_event_type='Lock')::BIGINT, \
                                COUNT(*)::BIGINT \
                         FROM pg_stat_activity \
                         WHERE application_name=$1 AND pid <> pg_backend_pid()",
                    )
                    .bind(&application_name)
                    .fetch_one(&pool)
                    .await;
                    match sample {
                        Ok((lock_waiters, matching_sessions)) => {
                            report.samples += 1;
                            if lock_waiters > 0 {
                                report.samples_with_waiters += 1;
                            }
                            report.max_lock_waiters = report.max_lock_waiters.max(lock_waiters);
                            report.max_matching_sessions =
                                report.max_matching_sessions.max(matching_sessions);
                        }
                        Err(error) => {
                            report.unavailable = Some(format!(
                                "pg_stat_activity sampling unavailable: {error}"
                            ));
                            return report;
                        }
                    }
                }
            }
        }
    }

    async fn database_deadlocks(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(
            "SELECT deadlocks::BIGINT FROM pg_stat_database WHERE datname=current_database()",
        )
        .fetch_one(pool)
        .await
    }

    #[derive(Debug)]
    struct WalletPlanSample {
        execution_ms: f64,
        shared_hit_blocks: i64,
        shared_read_blocks: i64,
        temp_blocks: i64,
        index_names: Vec<String>,
        node_types: Vec<String>,
    }

    fn collect_wallet_plan_nodes(plan: &Value, sample: &mut WalletPlanSample) {
        if let Some(node_type) = plan.get("Node Type").and_then(Value::as_str) {
            sample.node_types.push(node_type.to_owned());
        }
        if let Some(index_name) = plan.get("Index Name").and_then(Value::as_str) {
            sample.index_names.push(index_name.to_owned());
        }
        if let Some(children) = plan.get("Plans").and_then(Value::as_array) {
            for child in children {
                collect_wallet_plan_nodes(child, sample);
            }
        }
    }

    fn wallet_plan_sample(plan: Value) -> Result<WalletPlanSample, std::io::Error> {
        let root = plan
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or_else(|| std::io::Error::other("PostgreSQL EXPLAIN returned no plan"))?;
        let execution_ms = root
            .get("Execution Time")
            .and_then(Value::as_f64)
            .ok_or_else(|| std::io::Error::other("PostgreSQL EXPLAIN omitted Execution Time"))?;
        let root_plan = root
            .get("Plan")
            .ok_or_else(|| std::io::Error::other("PostgreSQL EXPLAIN omitted Plan"))?;
        let mut sample = WalletPlanSample {
            execution_ms,
            // PostgreSQL reports subtree totals on the root Plan node. Do not
            // sum child counters because that would count the same buffers more
            // than once.
            shared_hit_blocks: root_plan
                .get("Shared Hit Blocks")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            shared_read_blocks: root_plan
                .get("Shared Read Blocks")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            temp_blocks: root_plan
                .get("Temp Read Blocks")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                + root_plan
                    .get("Temp Written Blocks")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            index_names: Vec::new(),
            node_types: Vec::new(),
        };
        collect_wallet_plan_nodes(root_plan, &mut sample);
        Ok(sample)
    }

    async fn seed_wallet_plan_rows(
        pool: &sqlx::PgPool,
        target_wallet_id: i64,
        suffix: &str,
        start: i64,
        end: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO project_credit_ledger_entries \
             (wallet_id,amount_micros,entry_type,idempotency_key,metadata,created_at) \
             SELECT $1+(series%100),11,'admin_grant',$2||'-ledger-'||series,'{}', \
                    now()-make_interval(secs => (series%86400)::double precision) \
             FROM generate_series($3::bigint,$4::bigint) AS series",
        )
        .bind(target_wallet_id)
        .bind(suffix)
        .bind(start)
        .bind(end)
        .execute(pool)
        .await?;
        sqlx::query(
            "WITH inserted AS ( \
               INSERT INTO project_wallet_reservations \
                 (wallet_id,user_id,public_model_id,request_id,amount_micros,status,expires_at,created_at,updated_at) \
               SELECT $1+(series%100),0,0,$2||'-reservation-'||series,7, \
                      CASE WHEN series%100=0 THEN 'reserved' ELSE 'captured' END, \
                      CASE WHEN series%100=0 THEN now()+interval '1 hour' ELSE now()-interval '1 hour' END, \
                      now(),now() \
               FROM generate_series($3::bigint,$4::bigint) AS series \
               RETURNING id,wallet_id \
             ) \
             INSERT INTO project_wallet_reservation_allocations \
               (reservation_id,source_type,source_id,amount_micros,reserved_micros,allocation_class,created_at) \
             SELECT id,'project_credit',wallet_id,7,7,'PROJECT_CREDIT',now() FROM inserted",
        )
        .bind(target_wallet_id)
        .bind(suffix)
        .bind(start)
        .bind(end)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Opt-in query-plan baseline for the production wallet balance and active
    /// reservation reads. Data is spread over 100 wallet identities so the
    /// target wallet has realistic selective access at both tested scales.
    #[tokio::test]
    async fn postgres_wallet_ledger_and_reservation_plans_when_explicitly_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("CONDUIT_PG_BENCH").as_deref() != Ok("1") {
            return Ok(());
        }
        let dsn = std::env::var("CONDUIT_TEST_POSTGRES_DSN").map_err(|_| {
            std::io::Error::other("CONDUIT_TEST_POSTGRES_DSN is required when CONDUIT_PG_BENCH=1")
        })?;
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = &isolated.pool;
        let suffix = format!("wallet-plan-{}", uuid::Uuid::new_v4().simple());
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,status,description,profiles) \
             VALUES($1,'active','','{}'::jsonb) RETURNING id",
        )
        .bind(&suffix)
        .fetch_one(pool)
        .await?;
        let target_wallet_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
             VALUES($1,'STATION_CREDIT','active',now(),now()) RETURNING id",
        )
        .bind(project_id)
        .fetch_one(pool)
        .await?;

        let mut previous_rows = 0_i64;
        for total_rows in [1_000_i64, 10_000_i64] {
            seed_wallet_plan_rows(
                pool,
                target_wallet_id,
                &suffix,
                previous_rows + 1,
                total_rows,
            )
            .await?;
            previous_rows = total_rows;
            sqlx::query("ANALYZE project_credit_ledger_entries")
                .execute(pool)
                .await?;
            sqlx::query("ANALYZE project_wallet_reservations")
                .execute(pool)
                .await?;
            sqlx::query("ANALYZE project_wallet_reservation_allocations")
                .execute(pool)
                .await?;

            let expected = (total_rows / 100) * 11;
            let balance = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1",
            )
            .bind(target_wallet_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(balance, expected, "wallet ledger fixture/result mismatch");
            let ledger_plan: sqlx::types::Json<Value> = sqlx::query_scalar(
                "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) \
                 SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1",
            )
            .bind(target_wallet_id)
            .fetch_one(pool)
            .await?;
            let ledger = wallet_plan_sample(ledger_plan.0)?;

            let expected_reserved = (total_rows / 100) * 7;
            let admission_reserved = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(a.amount_micros),0)::BIGINT \
                 FROM project_wallet_reservation_allocations a \
                 JOIN project_wallet_reservations r ON r.id=a.reservation_id \
                 WHERE a.source_type='project_credit' AND a.source_id=$1 \
                   AND r.status='reserved' AND r.expires_at>$2",
            )
            .bind(target_wallet_id)
            .bind(Utc::now())
            .fetch_one(pool)
            .await?;
            assert_eq!(
                admission_reserved, expected_reserved,
                "admission reservation fixture/result mismatch"
            );
            let admission_plan: sqlx::types::Json<Value> = sqlx::query_scalar(
                "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) \
                 SELECT COALESCE(SUM(a.amount_micros),0)::BIGINT \
                 FROM project_wallet_reservation_allocations a \
                 JOIN project_wallet_reservations r ON r.id=a.reservation_id \
                 WHERE a.source_type='project_credit' AND a.source_id=$1 \
                   AND r.status='reserved' AND r.expires_at>$2",
            )
            .bind(target_wallet_id)
            .bind(Utc::now())
            .fetch_one(pool)
            .await?;
            let admission_reservations = wallet_plan_sample(admission_plan.0)?;

            let reserved = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(a.amount_micros),0)::BIGINT \
                 FROM project_wallet_reservation_allocations a \
                 JOIN project_wallet_reservations r ON r.id=a.reservation_id \
                 WHERE r.wallet_id=$1 AND a.source_type='project_credit' \
                   AND r.status IN ('reserved','shadow_reserved') AND r.expires_at>$2",
            )
            .bind(target_wallet_id)
            .bind(Utc::now())
            .fetch_one(pool)
            .await?;
            assert_eq!(
                reserved, expected_reserved,
                "wallet reservation fixture/result mismatch"
            );
            let reservation_plan: sqlx::types::Json<Value> = sqlx::query_scalar(
                "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) \
                 SELECT COALESCE(SUM(a.amount_micros),0)::BIGINT \
                 FROM project_wallet_reservation_allocations a \
                 JOIN project_wallet_reservations r ON r.id=a.reservation_id \
                 WHERE r.wallet_id=$1 AND a.source_type='project_credit' \
                   AND r.status IN ('reserved','shadow_reserved') AND r.expires_at>$2",
            )
            .bind(target_wallet_id)
            .bind(Utc::now())
            .fetch_one(pool)
            .await?;
            let reservations = wallet_plan_sample(reservation_plan.0)?;

            for (query_name, sample) in [
                ("ledger", &ledger),
                ("admission-reservations", &admission_reservations),
                ("settlement-reservations", &reservations),
            ] {
                assert!(
                    sample.execution_ms.is_finite() && sample.execution_ms < 5_000.0,
                    "{query_name} plan exceeded the 5s safety ceiling at {total_rows} rows: {sample:?}"
                );
                assert_eq!(
                    sample.temp_blocks, 0,
                    "{query_name} plan spilled temporary blocks at {total_rows} rows: {sample:?}"
                );
                assert!(
                    !sample.node_types.iter().any(|node| node == "Sort"),
                    "{query_name} aggregate unexpectedly sorted at {total_rows} rows: {sample:?}"
                );
            }
            assert!(
                ledger
                    .index_names
                    .iter()
                    .any(|name| name == "project_credit_ledger_entries_wallet"),
                "ledger plan did not use the existing wallet index at {total_rows} rows: {ledger:?}"
            );
            assert!(
                admission_reservations
                    .index_names
                    .iter()
                    .any(|name| name == "project_wallet_reservation_allocations_source"),
                "admission reservation plan did not use the allocation source index at {total_rows} rows: {admission_reservations:?}"
            );
            assert!(
                reservations.index_names.iter().any(|name| {
                    matches!(
                        name.as_str(),
                        "project_wallet_reservations_wallet_status"
                            | "project_wallet_reservation_allocation_source"
                            | "project_wallet_reservation_allocations_source"
                    )
                }),
                "reservation plan did not use an existing reservation/allocation index at {total_rows} rows: {reservations:?}"
            );
            println!(
                "postgres wallet plan: rows={total_rows} query=ledger execution_ms={:.3} \
                 shared_hit_blocks={} shared_read_blocks={} indexes={:?} nodes={:?}",
                ledger.execution_ms,
                ledger.shared_hit_blocks,
                ledger.shared_read_blocks,
                ledger.index_names,
                ledger.node_types,
            );
            println!(
                "postgres wallet plan: rows={total_rows} query=admission-reservations execution_ms={:.3} \
                 shared_hit_blocks={} shared_read_blocks={} indexes={:?} nodes={:?}",
                admission_reservations.execution_ms,
                admission_reservations.shared_hit_blocks,
                admission_reservations.shared_read_blocks,
                admission_reservations.index_names,
                admission_reservations.node_types,
            );
            println!(
                "postgres wallet plan: rows={total_rows} query=reservations execution_ms={:.3} \
                 shared_hit_blocks={} shared_read_blocks={} indexes={:?} nodes={:?}",
                reservations.execution_ms,
                reservations.shared_hit_blocks,
                reservations.shared_read_blocks,
                reservations.index_names,
                reservations.node_types,
            );
        }
        isolated.cleanup().await?;
        Ok(())
    }

    /// Opt-in contention benchmark for the production PostgreSQL settlement path.
    ///
    /// This is deliberately not part of the normal test budget. Run it with both
    /// `CONDUIT_PG_BENCH=1` and `CONDUIT_TEST_POSTGRES_DSN` set. Every operation
    /// settles against the same wallet and subscription bucket, so the benchmark
    /// exercises the row-lock ordering used to prevent overdraw and deadlocks.
    #[tokio::test]
    async fn postgres_concurrent_settlement_benchmark_when_explicitly_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("CONDUIT_PG_BENCH").as_deref() != Ok("1") {
            return Ok(());
        }
        let dsn = std::env::var("CONDUIT_TEST_POSTGRES_DSN").map_err(|_| {
            std::io::Error::other("CONDUIT_TEST_POSTGRES_DSN is required when CONDUIT_PG_BENCH=1")
        })?;
        let operations = std::env::var("CONDUIT_PG_BENCH_OPERATIONS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(64);
        let concurrency = std::env::var("CONDUIT_PG_BENCH_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(8);
        let timeout_secs = std::env::var("CONDUIT_PG_BENCH_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(120);
        let sample_interval_ms = std::env::var("CONDUIT_PG_BENCH_SAMPLE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(10);
        if operations == 0 || concurrency == 0 || sample_interval_ms == 0 {
            return Err(std::io::Error::other(
                "CONDUIT_PG_BENCH_OPERATIONS, CONDUIT_PG_BENCH_CONCURRENCY, and \
                 CONDUIT_PG_BENCH_SAMPLE_MS must be positive",
            )
            .into());
        }

        const CHARGE_MICROS: i64 = 1_000_000;
        let initial_funds = i64::try_from(operations)?
            .checked_mul(CHARGE_MICROS)
            .ok_or_else(|| std::io::Error::other("benchmark funding overflow"))?;
        let initial_subscription = initial_funds / 2;
        let initial_wallet = initial_funds - initial_subscription;
        let fixture = FundsFixture::new(&dsn, initial_subscription, 0, initial_wallet).await?;

        // Fixture creation is intentionally outside the timed region. Charge events
        // are also prepared first so this measures only the contested settlement
        // transaction, not setup INSERTs.
        let usage_base = (uuid::Uuid::new_v4().as_u128() % (i64::MAX as u128 / 2)) as i64;
        let mut work = Vec::with_capacity(operations);
        for offset in 0..operations {
            let usage_log_id = usage_base + i64::try_from(offset)?;
            work.push((usage_log_id, fixture.charge_event(usage_log_id).await?));
        }

        // pg_stat_activity does not expose the current search_path as a reliable
        // workload label. Stamp each benchmark transaction with a unique,
        // transaction-local application_name so concurrent tests and normal
        // Conduit API traffic are excluded from lock-wait samples.
        let benchmark_application_name =
            format!("conduit_settlement_bench_{}", uuid::Uuid::new_v4().simple());
        let monitor_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&dsn)
            .await?;
        let deadlocks_before = database_deadlocks(&monitor_pool).await;
        let (sampler_stop, sampler_stop_rx) = tokio::sync::watch::channel(false);
        let sampler = tokio::spawn(sample_benchmark_lock_waits(
            monitor_pool.clone(),
            benchmark_application_name.clone(),
            std::time::Duration::from_millis(sample_interval_ms),
            sampler_stop_rx,
        ));

        let limiter = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut tasks = tokio::task::JoinSet::new();
        let workload_started = std::time::Instant::now();
        for (usage_log_id, event_id) in work {
            let limiter = limiter.clone();
            let pool = fixture.isolated.pool.clone();
            let incurred_at = fixture.incurred_at;
            let user_id = fixture.user_id;
            let project_id = fixture.project_id;
            let model_id = fixture.model_id;
            let benchmark_application_name = benchmark_application_name.clone();
            tasks.spawn(async move {
                let _permit = limiter
                    .acquire_owned()
                    .await
                    .map_err(|error| error.to_string())?;
                let started = std::time::Instant::now();
                let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
                sqlx::query("SELECT set_config('application_name',$1,true)")
                    .bind(benchmark_application_name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| error.to_string())?;
                // A lock wait that exceeds this bound is a benchmark failure rather
                // than an unbounded hang. PostgreSQL deadlock errors also propagate.
                sqlx::query("SET LOCAL lock_timeout = '30s'")
                    .execute(&mut *tx)
                    .await
                    .map_err(|error| error.to_string())?;
                settle_funds(
                    &mut tx,
                    event_id,
                    usage_log_id,
                    user_id,
                    project_id,
                    model_id,
                    STATION_CREDIT_CODE,
                    CHARGE_MICROS,
                    incurred_at,
                )
                .await?;
                tx.commit().await.map_err(|error| error.to_string())?;
                Ok::<_, String>(started.elapsed())
            });
        }

        let collect = async {
            let mut latencies = Vec::with_capacity(operations);
            while let Some(result) = tasks.join_next().await {
                let latency = result
                    .map_err(|error| std::io::Error::other(error.to_string()))?
                    .map_err(std::io::Error::other)?;
                latencies.push(latency);
            }
            Ok::<_, Box<dyn std::error::Error>>(latencies)
        };
        let workload_result =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), collect)
                .await
                .map_err(|_| {
                    std::io::Error::other(format!(
                        "settlement benchmark exceeded {timeout_secs}s; possible lock stall"
                    ))
                });
        let _ = sampler_stop.send(true);
        let lock_wait_samples = sampler
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let deadlocks_after = database_deadlocks(&monitor_pool).await;
        monitor_pool.close().await;
        let mut latencies = workload_result??;
        let elapsed = workload_started.elapsed();
        latencies.sort_unstable();
        let percentile = |percent: usize| {
            let index = (latencies.len() * percent).div_ceil(100).saturating_sub(1);
            latencies[index].as_micros()
        };
        let throughput = operations as f64 / elapsed.as_secs_f64();

        let (bucket_consumed, bucket_reserved): (i64, i64) = sqlx::query_as(
            "SELECT consumed_micros,reserved_micros \
             FROM subscription_allowance_buckets WHERE id=$1",
        )
        .bind(fixture.bucket_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        let wallet_balance = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
             FROM project_credit_ledger_entries WHERE wallet_id=$1",
        )
        .bind(fixture.wallet_id)
        .fetch_one(&fixture.isolated.pool)
        .await?;
        let (settled_count, settled_amount): (i64, i64) = sqlx::query_as(
            "SELECT COUNT(*)::BIGINT,COALESCE(SUM(amount_micros),0)::BIGINT \
             FROM charge_settlements WHERE status='settled'",
        )
        .fetch_one(&fixture.isolated.pool)
        .await?;
        let insufficient_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::BIGINT FROM charge_settlements \
             WHERE status='insufficient_funds'",
        )
        .fetch_one(&fixture.isolated.pool)
        .await?;

        println!(
            "postgres settlement benchmark: operations={operations} concurrency={concurrency} \
             throughput={throughput:.2} tx/s p50={}us p95={}us p99={}us max={}us",
            percentile(50),
            percentile(95),
            percentile(99),
            latencies.last().map_or(0, |duration| duration.as_micros()),
        );
        if let Some(reason) = &lock_wait_samples.unavailable {
            println!("postgres settlement benchmark lock-wait diagnostics unavailable: {reason}");
        } else if lock_wait_samples.samples == 0 {
            println!(
                "postgres settlement benchmark lock-wait diagnostics unavailable: no successful samples"
            );
        } else {
            println!(
                "postgres settlement benchmark: lock_wait_max={} \
                 lock_wait_sample_ratio={:.4} lock_samples={} max_matching_sessions={}",
                lock_wait_samples.max_lock_waiters,
                lock_wait_samples.waiter_sample_ratio(),
                lock_wait_samples.samples,
                lock_wait_samples.max_matching_sessions,
            );
        }
        match (deadlocks_before, deadlocks_after) {
            (Ok(before), Ok(after)) if after >= before => println!(
                "postgres settlement benchmark: deadlocks_delta={} (database-wide)",
                after - before
            ),
            (Ok(_), Ok(_)) => println!(
                "postgres settlement benchmark deadlock diagnostics unavailable: \
                 pg_stat_database counter reset during benchmark"
            ),
            (Err(error), _) | (_, Err(error)) => {
                println!("postgres settlement benchmark deadlock diagnostics unavailable: {error}")
            }
        }

        assert_eq!(latencies.len(), operations, "every task must complete");
        assert_eq!(settled_count, i64::try_from(operations)?);
        assert_eq!(insufficient_count, 0, "all seeded funds should settle");
        assert_eq!(settled_amount, initial_funds);
        assert_eq!(bucket_reserved, 0);
        assert_eq!(
            bucket_consumed + (initial_wallet - wallet_balance),
            settled_amount,
            "subscription consumption plus wallet debits must equal settlements"
        );
        assert_eq!(
            initial_funds,
            (initial_subscription - bucket_consumed) + wallet_balance + settled_amount,
            "initial funds must equal remaining funds plus settled charges"
        );

        fixture.isolated.cleanup().await?;
        Ok(())
    }
}

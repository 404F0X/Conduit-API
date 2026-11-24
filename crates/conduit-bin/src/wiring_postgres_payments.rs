//! Project-wallet topups with the explicitly selected classic-MD5 reference profile.
use async_graphql::ID;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::payments::{
    CreatePaymentOrderInput, OperatorPaymentAccess, OperatorPaymentOrder, PaymentCapabilities,
    PaymentChannel, PaymentOrder, PaymentServices,
};
use conduit_core::objects::money::{AccountingSettings, STATION_CREDIT_CODE};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgPaymentAdapter {
    pool: PgPool,
    config: conduit_config::model::PaymentConfig,
    gateway: Option<crate::epay_classic_md5::EpayGateway>,
}
impl PgPaymentAdapter {
    #[cfg(test)]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            config: Default::default(),
            gateway: None,
        }
    }
    pub fn from_config(
        pool: PgPool,
        config: conduit_config::model::PaymentConfig,
    ) -> Result<Self, String> {
        let gateway = if config.enabled {
            Some(crate::epay_classic_md5::EpayGateway::new(config.clone())?)
        } else {
            None
        };
        Ok(Self {
            pool,
            config,
            gateway,
        })
    }
    fn present(&self, row: sqlx::postgres::PgRow) -> Result<PaymentOrder, String> {
        let eligible = row.get::<String, _>("status") == "pending"
            && row.get::<DateTime<Utc>, _>("expires_at") > Utc::now();
        let matching = self.gateway.as_ref().is_some_and(|g| {
            g.identity() == row.get::<String, _>("merchant_identity")
                && g.snapshot() == row.get::<String, _>("provider_config")
        });
        let mut order = public_order(row)?;
        if eligible && matching {
            order.checkout_url = Some(self.gateway.as_ref().ok_or("payments disabled")?.checkout(
                Uuid::parse_str(order.id.as_str()).map_err(|_| "invalid payment identity")?,
                parse_minor(&order.amount_minor)?,
                order.channel.as_str(),
            )?);
        }
        Ok(order)
    }
    async fn reconcile(
        &self,
        id: Uuid,
        notify: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<(), String> {
        let gateway = self.gateway.as_ref().ok_or("payments disabled")?;
        let row = sqlx::query(
            "SELECT merchant_identity,provider_config,status FROM payment_orders WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        if row.get::<String, _>("status") == "credited" {
            return Ok(());
        }
        if gateway.identity() != row.get::<String, _>("merchant_identity")
            || gateway.snapshot() != row.get::<String, _>("provider_config")
        {
            return Err("payment merchant configuration unavailable; restore the frozen configuration to reconcile".into());
        }
        let queried = gateway.query(id).await?;
        if !queried.paid {
            return if notify.is_some() {
                Err("successful notification not confirmed by payment query".into())
            } else {
                Ok(())
            };
        }
        let mut mismatch =
            queried.merchant_id != gateway.merchant_id() || queried.order_id != id.to_string();
        let verification_context = serde_json::json!({"queryMerchantID":queried.merchant_id,"queryOrderID":queried.order_id,"notification":notify.map(|n|n.iter().filter(|(key,_)|matches!(key.as_str(),"pid"|"out_trade_no"|"trade_no"|"type"|"money"|"trade_status")).map(|(k,v)|(k.clone(),v.clone())).collect::<std::collections::BTreeMap<_,_>>())});
        if let Some(n) = notify {
            mismatch |= n.get("pid") != Some(&queried.merchant_id)
                || n.get("out_trade_no") != Some(&queried.order_id)
                || n.get("trade_no") != Some(&queried.transaction_id)
                || n.get("type")
                    .and_then(|s| crate::epay_classic_md5::local_channel(s).ok())
                    != Some(queried.channel.as_str())
                || n.get("money")
                    .and_then(|s| crate::epay_classic_md5::money_minor(s).ok())
                    != Some(queried.amount_minor);
        }
        apply_verified_paid(
            &self.pool,
            VerifiedPaid {
                order_id: id,
                merchant_identity: gateway.identity().into(),
                transaction_id: queried.transaction_id,
                channel: queried.channel,
                currency: "CNY".into(),
                amount_minor: queried.amount_minor,
                paid_at: queried.paid_at.ok_or("paid time missing")?,
                verification_mismatch: mismatch,
                verification_context,
            },
        )
        .await?;
        Ok(())
    }
    async fn member(&self, payer: i64, project: i64) -> Result<(), String> {
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_projects up JOIN users u ON u.id=up.user_id JOIN projects p ON p.id=up.project_id WHERE up.user_id=$1 AND up.project_id=$2 AND u.status='activated' AND u.deleted_at=0 AND p.status='active' AND p.deleted_at=0)")
            .bind(payer).bind(project).fetch_one(&self.pool).await.map_err(storage)?;
        if active {
            Ok(())
        } else {
            Err("active Project membership required".into())
        }
    }
}
fn operator_order(row: sqlx::postgres::PgRow) -> Result<OperatorPaymentOrder, String> {
    Ok(OperatorPaymentOrder {
        payer_user_id: ID(row.get::<i64, _>("payer_user_id").to_string()),
        // Operators inspect frozen receipts, never another payer's cashier URL.
        order: public_order(row)?,
    })
}
fn public_order(row: sqlx::postgres::PgRow) -> Result<PaymentOrder, String> {
    let snapshot: FrozenTopup = serde_json::from_str(&row.get::<String, _>("frozen_accounting"))
        .map_err(|_| "payment snapshot unavailable")?;
    Ok(PaymentOrder {
        id: ID(row.get::<Uuid, _>("id").to_string()),
        project_id: ID(row.get::<i64, _>("project_id").to_string()),
        status: row.get("status"),
        channel: match row.get::<String, _>("channel").as_str() {
            "alipay" => PaymentChannel::Alipay,
            "wechat" => PaymentChannel::Wechat,
            _ => return Err("payment channel unavailable".into()),
        },
        currency: row.get("currency"),
        amount_minor: row.get::<i64, _>("amount_minor").to_string(),
        credits_micros: row.get::<i64, _>("credits_micros").to_string(),
        recharge_multiplier_ppm: snapshot.recharge_multiplier_ppm.to_string(),
        accounting_version: snapshot.accounting.version.to_string(),
        created_at: row.get::<DateTime<Utc>, _>("created_at").to_rfc3339(),
        expires_at: row.get::<DateTime<Utc>, _>("expires_at").to_rfc3339(),
        paid_at: row
            .get::<Option<DateTime<Utc>>, _>("paid_at")
            .map(|d| d.to_rfc3339()),
        checkout_url: None,
        review_reason: row.get("review_reason"),
    })
}
#[async_trait::async_trait]
impl conduit_http::payment_handlers::PaymentNotificationService for PgPaymentAdapter {
    async fn notify(&self, raw_query: &str) -> Result<(), String> {
        let gateway = self.gateway.as_ref().ok_or("payments disabled")?;
        let fields = gateway.notify(raw_query)?;
        let id = Uuid::parse_str(
            fields
                .get("out_trade_no")
                .ok_or("payment order identity missing")?,
        )
        .map_err(|_| "payment order identity invalid")?;
        self.reconcile(id, Some(&fields)).await
    }
}
#[async_trait::async_trait]
impl PaymentServices for PgPaymentAdapter {
    async fn operator_orders(
        &self,
        _access: OperatorPaymentAccess,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<OperatorPaymentOrder>, String> {
        if !(1..=100).contains(&limit) || offset < 0 {
            return Err("invalid payment pagination".into());
        }
        sqlx::query("SELECT * FROM payment_orders ORDER BY created_at DESC,id LIMIT $1 OFFSET $2")
            .bind(i64::from(limit))
            .bind(i64::from(offset))
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
            .into_iter()
            .map(operator_order)
            .collect()
    }
    async fn operator_order(
        &self,
        _access: OperatorPaymentAccess,
        id: &str,
    ) -> Result<OperatorPaymentOrder, String> {
        let id = Uuid::parse_str(id).map_err(|_| "invalid payment order identity")?;
        let row = sqlx::query("SELECT * FROM payment_orders WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?
            .ok_or("payment order not found")?;
        operator_order(row)
    }
    async fn operator_recheck(
        &self,
        access: OperatorPaymentAccess,
        id: &str,
    ) -> Result<OperatorPaymentOrder, String> {
        access.require_write()?;
        self.reconcile(
            Uuid::parse_str(id).map_err(|_| "invalid payment order identity")?,
            None,
        )
        .await?;
        self.operator_order(access, id).await
    }
    fn capabilities(&self) -> PaymentCapabilities {
        PaymentCapabilities {
            enabled: self.gateway.is_some(),
            currency: "CNY".into(),
            channels: if self.gateway.is_some() {
                vec![PaymentChannel::Alipay, PaymentChannel::Wechat]
            } else {
                vec![]
            },
            min_amount_minor: self.config.min_amount_minor.to_string(),
            max_amount_minor: self.config.max_amount_minor.to_string(),
        }
    }
    async fn create(
        &self,
        payer: i64,
        project: i64,
        input: CreatePaymentOrderInput,
    ) -> Result<PaymentOrder, String> {
        self.member(payer, project).await?;
        let gateway = self.gateway.as_ref().ok_or("payments disabled")?;
        let amount = parse_minor(&input.amount_minor)?;
        if amount < self.config.min_amount_minor || amount > self.config.max_amount_minor {
            return Err("payment amount outside configured limits".into());
        }
        let id = create_local_order(
            &self.pool,
            NewPaymentOrder {
                payer,
                project,
                amount_minor: amount,
                channel: input.channel.as_str().into(),
                idempotency_key: input.idempotency_key,
                merchant_identity: gateway.identity().into(),
                provider_config: gateway.snapshot(),
                expires_at: Utc::now()
                    + chrono::Duration::seconds(self.config.order_ttl_seconds as i64),
            },
        )
        .await?;
        self.order(payer, project, &id.to_string()).await
    }
    async fn orders(
        &self,
        payer: i64,
        project: i64,
        limit: i32,
        offset: i32,
    ) -> Result<Vec<PaymentOrder>, String> {
        self.member(payer, project).await?;
        if !(1..=100).contains(&limit) || offset < 0 {
            return Err("invalid payment pagination".into());
        }
        sqlx::query("SELECT * FROM payment_orders WHERE payer_user_id=$1 AND project_id=$2 ORDER BY created_at DESC,id LIMIT $3 OFFSET $4")
            .bind(payer).bind(project).bind(i64::from(limit)).bind(i64::from(offset)).fetch_all(&self.pool).await.map_err(storage)?
            .into_iter().map(|row|self.present(row)).collect()
    }
    async fn order(&self, payer: i64, project: i64, id: &str) -> Result<PaymentOrder, String> {
        self.member(payer, project).await?;
        let id = Uuid::parse_str(id).map_err(|_| "invalid payment order identity")?;
        let row = sqlx::query(
            "SELECT * FROM payment_orders WHERE id=$1 AND payer_user_id=$2 AND project_id=$3",
        )
        .bind(id)
        .bind(payer)
        .bind(project)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or("payment order not found")?;
        self.present(row)
    }
    async fn recheck(&self, payer: i64, project: i64, id: &str) -> Result<PaymentOrder, String> {
        self.order(payer, project, id).await?;
        self.reconcile(
            Uuid::parse_str(id).map_err(|_| "invalid payment identity")?,
            None,
        )
        .await?;
        self.order(payer, project, id).await
    }
}

pub const MAX_RECHARGE_MULTIPLIER_PPM: i64 = 1_000_000_000;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrozenTopup {
    pub formula_version: u32,
    pub currency: String,
    pub amount_minor: i64,
    pub accounting: AccountingSettings,
    pub recharge_multiplier_ppm: i64,
    pub group_id: Option<String>,
    pub group_revision: Option<i64>,
    pub credits_micros: i64,
}
pub fn parse_minor(raw: &str) -> Result<i64, String> {
    if raw.is_empty()
        || raw.len() > 19
        || !raw.bytes().all(|b| b.is_ascii_digit())
        || raw.starts_with('0')
    {
        return Err("amountMinor must be a positive canonical integer CNY fen".into());
    }
    raw.parse::<i64>()
        .map_err(|_| "amountMinor exceeds supported range".into())
}
pub fn freeze_topup(
    settings: AccountingSettings,
    amount_minor: i64,
    ppm: i64,
    group_id: Option<String>,
    group_revision: Option<i64>,
) -> Result<FrozenTopup, String> {
    if amount_minor <= 0 || !(1..=MAX_RECHARGE_MULTIPLIER_PPM).contains(&ppm) {
        return Err("invalid topup amount or recharge multiplier".into());
    }
    settings.validate()?;
    // Checked equivalents of canonical real_to_accounting/accounting_to_credits:
    // the canonical methods use unchecked Decimal operators, which may panic.
    let quote = if settings.accounting_currency.eq_ignore_ascii_case("CNY") {
        Decimal::ONE
    } else {
        settings
            .exchange_rates
            .iter()
            .find(|r| r.currency.eq_ignore_ascii_case("CNY"))
            .ok_or("missing exchange rate for CNY")?
            .quote_per_accounting_unit
    };
    let credits_micros = Decimal::from(amount_minor)
        .checked_div(Decimal::from(100))
        .and_then(|v| v.checked_div(quote))
        .and_then(|v| v.checked_mul(settings.credits_per_accounting_unit))
        .and_then(|v| v.checked_mul(Decimal::from(ppm)))
        .and_then(|v| v.floor().to_i64())
        .filter(|v| *v > 0)
        .ok_or("topup credits overflow or round to zero")?;
    Ok(FrozenTopup {
        formula_version: 1,
        currency: "CNY".into(),
        amount_minor,
        accounting: settings,
        recharge_multiplier_ppm: ppm,
        group_id,
        group_revision,
        credits_micros,
    })
}
fn storage(_: sqlx::Error) -> String {
    "payment storage operation failed".into()
}

#[derive(Clone)]
pub(crate) struct NewPaymentOrder {
    pub payer: i64,
    pub project: i64,
    pub amount_minor: i64,
    pub channel: String,
    pub idempotency_key: String,
    /// Server-owned immutable configuration version, never a secret/key hash.
    pub merchant_identity: String,
    pub provider_config: String,
    pub expires_at: DateTime<Utc>,
}

/// Persists the order before any cashier interaction. Caller must have selected
/// a complete, verified and enabled provider configuration.
pub(crate) async fn create_local_order(
    pool: &PgPool,
    input: NewPaymentOrder,
) -> Result<Uuid, String> {
    if input.amount_minor <= 0
        || !matches!(input.channel.as_str(), "alipay" | "wechat")
        || input.idempotency_key.is_empty()
        || input.idempotency_key.len() > 128
        || !input
            .idempotency_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || input.merchant_identity.is_empty()
        || input.merchant_identity.len() > 128
        || input.expires_at <= Utc::now()
    {
        return Err("invalid payment order input".into());
    }
    let mut tx = pool.begin().await.map_err(storage)?;
    // Serialize the payer's input key, including the first creation race.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "payment-order:{}:{}",
            input.payer, input.idempotency_key
        ))
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    let member: Option<i64> = sqlx::query_scalar("SELECT up.user_id FROM user_projects up JOIN users u ON u.id=up.user_id JOIN projects p ON p.id=up.project_id WHERE up.user_id=$1 AND up.project_id=$2 AND u.status='activated' AND u.deleted_at=0 AND p.status='active' AND p.deleted_at=0 FOR SHARE OF up,u,p")
        .bind(input.payer).bind(input.project).fetch_optional(&mut *tx).await.map_err(storage)?;
    if member.is_none() {
        return Err("active Project membership required".into());
    }
    if let Some(row) = sqlx::query("SELECT id,project_id,amount_minor,channel FROM payment_orders WHERE payer_user_id=$1 AND idempotency_key=$2")
        .bind(input.payer).bind(&input.idempotency_key).fetch_optional(&mut *tx).await.map_err(storage)? {
        if row.get::<i64,_>("project_id") != input.project || row.get::<i64,_>("amount_minor") != input.amount_minor || row.get::<String,_>("channel") != input.channel {
            return Err("payment idempotency input conflict".into());
        }
        let id = row.get("id");
        tx.commit().await.map_err(storage)?;
        return Ok(id);
    }
    let group: Option<(String,String,i64,i64)> = sqlx::query_as("SELECT g.id,g.status,g.recharge_multiplier_ppm,g.recharge_revision FROM user_group_users m JOIN user_groups g ON g.id=m.user_group_id WHERE m.user_id=$1 FOR SHARE OF m,g")
        .bind(input.payer).fetch_optional(&mut *tx).await.map_err(storage)?;
    if group
        .as_ref()
        .is_some_and(|(_, status, _, _)| status != "enabled")
    {
        return Err("payer recharge group unavailable".into());
    }
    let settings = crate::usage_charge_settler_postgres::load_accounting_settings_from(&mut *tx)
        .await
        .map_err(|_| "payment accounting settings unavailable")?;
    let snapshot = freeze_topup(
        settings,
        input.amount_minor,
        group.as_ref().map_or(1_000_000, |g| g.2),
        group.as_ref().map(|g| g.0.clone()),
        group.as_ref().map(|g| g.3),
    )?;
    let snapshot_json =
        serde_json::to_string(&snapshot).map_err(|_| "payment snapshot serialization failed")?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO payment_orders(id,payer_user_id,project_id,idempotency_key,provider,merchant_identity,channel,currency,amount_minor,credits_micros,frozen_accounting,status,expires_at,provider_config,created_at,updated_at) VALUES($1,$2,$3,$4,'epay-classic-md5',$5,$6,'CNY',$7,$8,$9,'pending',$10,$11,now(),now())")
        .bind(id).bind(input.payer).bind(input.project).bind(input.idempotency_key).bind(input.merchant_identity).bind(input.channel).bind(input.amount_minor).bind(snapshot.credits_micros).bind(snapshot_json).bind(input.expires_at).bind(input.provider_config)
        .execute(&mut *tx).await.map_err(storage)?;
    sqlx::query(
        "INSERT INTO payment_order_events(order_id,outcome,created_at) VALUES($1,'pending',now())",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(id)
}

/// Constructed only by the adapter after an authenticated server query;
/// notifications additionally require the reference-profile signature.
/// Kept crate-private so no HTTP/GraphQL deserializer can mint this authority.
#[derive(Clone)]
pub(crate) struct VerifiedPaid {
    pub order_id: Uuid,
    pub merchant_identity: String,
    pub transaction_id: String,
    pub channel: String,
    pub currency: String,
    pub amount_minor: i64,
    pub paid_at: DateTime<Utc>,
    pub verification_mismatch: bool,
    pub verification_context: serde_json::Value,
}

/// Atomically records the durable verified outcome and the one wallet credit.
/// Network calls must finish before entering this transaction.
pub(crate) async fn apply_verified_paid(
    pool: &PgPool,
    paid: VerifiedPaid,
) -> Result<String, String> {
    let mut tx = pool.begin().await.map_err(storage)?;
    if paid.transaction_id.is_empty()
        || paid.transaction_id.len() > 128
        || paid.merchant_identity.len() > 128
    {
        return Err("invalid verified transaction identity".into());
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "payment-transaction:{}:{}",
            paid.merchant_identity, paid.transaction_id
        ))
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    let row = sqlx::query("SELECT * FROM payment_orders WHERE id=$1 FOR UPDATE")
        .bind(paid.order_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
    let status: String = row.get("status");
    if status == "credited" {
        tx.commit().await.map_err(storage)?;
        return Ok(status);
    }
    let mismatch = paid.verification_mismatch
        || row
            .get::<Option<String>, _>("provider_transaction_id")
            .as_ref()
            .is_some_and(|id| id != &paid.transaction_id)
        || paid.paid_at + chrono::Duration::seconds(1) < row.get::<DateTime<Utc>, _>("created_at")
        || row.get::<String, _>("merchant_identity") != paid.merchant_identity
        || row.get::<String, _>("channel") != paid.channel
        || row.get::<String, _>("currency") != paid.currency
        || row.get::<i64, _>("amount_minor") != paid.amount_minor
        || paid.transaction_id.is_empty()
        || paid.transaction_id.len() > 128;
    let project: i64 = row.get("project_id");
    let target: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM projects WHERE id=$1 AND status='active' AND deleted_at=0 FOR SHARE",
    )
    .bind(project)
    .fetch_optional(&mut *tx)
    .await
    .map_err(storage)?;
    let expiry: DateTime<Utc> = row.get("expires_at");
    let mut reason = if mismatch {
        Some("payment_mismatch")
    } else if target.is_none() {
        Some("target_closed")
    } else if paid.paid_at > expiry || status == "closed" {
        Some("late_paid")
    } else if status == "review_required" {
        Some("manual_review_required")
    } else {
        None
    };
    let duplicate: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM payment_orders WHERE merchant_identity=$1 AND provider_transaction_id=$2 AND id<>$3)")
        .bind(&paid.merchant_identity).bind(&paid.transaction_id).bind(paid.order_id).fetch_one(&mut *tx).await.map_err(storage)?;
    if duplicate {
        reason = Some("transaction_reused");
    }
    let wallet: Option<(i64, String)> = if reason.is_none() {
        sqlx::query("INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) VALUES($1,$2,'active',now(),now()) ON CONFLICT(project_id,currency) DO NOTHING")
            .bind(project).bind(STATION_CREDIT_CODE).execute(&mut *tx).await.map_err(storage)?;
        sqlx::query_as(
            "SELECT id,status FROM project_wallets WHERE project_id=$1 AND currency=$2 FOR UPDATE",
        )
        .bind(project)
        .bind(STATION_CREDIT_CODE)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
    } else {
        None
    };
    if reason.is_none() && wallet.as_ref().is_none_or(|(_, s)| s != "active") {
        reason = Some("target_closed");
    }
    let ledger: Option<i64> = if reason.is_none() {
        Some(sqlx::query_scalar("INSERT INTO project_credit_ledger_entries(wallet_id,amount_micros,entry_type,reference_type,reference_id,idempotency_key,description,metadata,created_at) VALUES($1,$2,'payment_topup','payment_order',$3,$4,'Payment topup','{}',now()) RETURNING id")
            .bind(wallet.as_ref().ok_or("wallet unavailable")?.0).bind(row.get::<i64,_>("credits_micros"))
            .bind(paid.order_id.to_string()).bind(format!("payment-topup:{}", paid.order_id)).fetch_one(&mut *tx).await.map_err(storage)?)
    } else {
        None
    };
    let next = if reason.is_some() {
        "review_required"
    } else {
        "credited"
    };
    sqlx::query("UPDATE payment_orders SET status=$2,review_reason=$3,ledger_entry_id=$4,provider_transaction_id=COALESCE(provider_transaction_id,CASE WHEN $5 THEN NULL ELSE $6 END),paid_at=COALESCE(paid_at,$7),updated_at=now() WHERE id=$1")
        .bind(paid.order_id).bind(next).bind(reason).bind(ledger).bind(duplicate).bind(&paid.transaction_id).bind(paid.paid_at).execute(&mut *tx).await.map_err(storage)?;
    let evidence = serde_json::json!({"merchantIdentity":paid.merchant_identity,"transactionID":paid.transaction_id,"currency":paid.currency,"channel":paid.channel,"amountMinor":paid.amount_minor.to_string(),"paidAt":paid.paid_at.to_rfc3339(),"verification":paid.verification_context});
    sqlx::query("INSERT INTO payment_order_events(order_id,outcome,reason,verified_evidence,created_at) VALUES($1,$2,$3,$4,now())")
        .bind(paid.order_id).bind(next).bind(reason).bind(evidence).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(next.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn postgres_reference_gateway_notify_http_and_query_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        use conduit_http::payment_handlers::PaymentNotificationService;
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path, query_param},
        };
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let gateway_server = MockServer::start().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let callback_address = listener.local_addr()?;
        let pool = database.pool.clone();
        // Only this test bypasses the production HTTPS configuration validator.
        let config = conduit_config::model::PaymentConfig {
            enabled: true,
            gateway_base_url: format!("{}/", gateway_server.uri()),
            merchant_id: "7".into(),
            merchant_key: "test-key".into(),
            merchant_identity: "fixture-v1".into(),
            notify_url: "https://app.example.test/api/payments/epay/notify".into(),
            return_url: "https://app.example.test/wallet".into(),
            ..Default::default()
        };
        let adapter = std::sync::Arc::new(PgPaymentAdapter {
            pool: pool.clone(),
            gateway: Some(crate::epay_classic_md5::EpayGateway::fixture(
                config.clone(),
            )?),
            config,
        });
        let state = conduit_http::AppState::new(
            std::sync::Arc::new(conduit_config::AppConfig::default()),
            std::sync::Arc::new(
                conduit_http::AppServices::new().with_payment_notification_service(adapter.clone()),
            ),
        );
        let router = conduit_http::router::build_router(state);
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let result:Result<(),Box<dyn std::error::Error>>=async {
            database.owners(&[9002,9003]).await?;
            let project:i64=sqlx::query_scalar("INSERT INTO projects(name,status,profiles) VALUES('Gateway fixture','active','{}'::jsonb) RETURNING id").fetch_one(&pool).await?;
            sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES(9002,$1,true,'[]'::jsonb),(9003,$1,false,'[]'::jsonb)").bind(project).execute(&pool).await?;
            let schema=conduit_admin_graphql::admin_schema_builder()
                .extension(conduit_admin_graphql::authz_extension::ScopeAuthExtensionFactory)
                .data(adapter.clone() as std::sync::Arc<dyn PaymentServices>)
                .data(std::sync::Arc::new(crate::wiring_governance::PgGovernanceAdapter::new(pool.clone())) as std::sync::Arc<dyn conduit_admin_graphql::governance::GovernanceServices>)
                .data(conduit_admin_graphql::me::CurrentUser {user_id:9003}).finish();
            let mut payer_context=conduit_auth::request_context::RequestContext::new();
            payer_context.set_principal(conduit_auth::Principal::user("9002"))?;
            payer_context.set_project_id(project.to_string())?;
            let created=schema.execute(async_graphql::Request::new(r#"mutation { createPaymentOrder(input:{amountMinor:"1",channel:WECHAT,idempotencyKey:"gateway-one"}) { id } }"#)
                .data(payer_context.clone()).data(conduit_admin_graphql::me::CurrentUser {user_id:9002})).await;
            assert!(created.errors.is_empty(),"{:?}",created.errors);
            let created=created.data.into_json()?;
            let order=adapter.order(9002,project,created["createPaymentOrder"]["id"].as_str().ok_or("created id missing")?).await?;
            let id=Uuid::parse_str(order.id.as_str())?;
            assert_eq!(order.credits_micros,"100000000");
            let checkout=url::Url::parse(order.checkout_url.as_deref().ok_or("checkout missing")?)?;
            let fields=crate::epay_classic_md5::parse_query(checkout.query().ok_or("checkout query missing")?)?;
            assert_eq!(fields["type"],"wxpay");
            assert_eq!(fields["money"],"0.01");
            assert_eq!(fields["sign"],crate::epay_classic_md5::signature(&fields,"test-key"));
            // Each simulated payment must follow creation of its own order.
            let paid_time=|| Utc::now().with_timezone(&chrono_tz::Asia::Shanghai).format("%Y-%m-%d %H:%M:%S").to_string();
            Mock::given(method("GET")).and(path("/api.php")).and(query_param("act","order")).and(query_param("pid","7")).and(query_param("key","test-key")).and(query_param("out_trade_no",id.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":1,"status":1,"pid":"7","out_trade_no":id.to_string(),"trade_no":"gateway-txn-1","api_trade_no":"not-the-ledger-identity","type":"wxpay","money":"0.01","endtime":paid_time()}))).mount(&gateway_server).await;
            let mut notice=std::collections::BTreeMap::from([("pid".into(),"7".into()),("out_trade_no".into(),id.to_string()),("trade_no".into(),"gateway-txn-1".into()),("type".into(),"wxpay".into()),("money".into(),"0.01".into()),("name".into(),"中文 +%&=".into()),("trade_status".into(),"TRADE_SUCCESS".into()),("sign_type".into(),"MD5".into())]);
            notice.insert("sign".into(),crate::epay_classic_md5::signature(&notice,"test-key"));
            let raw=url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&notice).finish();
            assert!(adapter.notify(&(raw.clone()+"&pid=7")).await.is_err());
            let mut invalid=notice.clone();invalid.insert("sign".into(),"0".repeat(32));
            let invalid_raw=url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&invalid).finish();
            assert!(adapter.notify(&invalid_raw).await.is_err());
            assert!(gateway_server.received_requests().await.unwrap().is_empty());
            let payer_recheck=format!("mutation {{ recheckPaymentOrder(id: \"{}\") {{ status }} }}",order.id.as_str());
            let checked=schema.execute(async_graphql::Request::new(payer_recheck)
                .data(payer_context).data(conduit_admin_graphql::me::CurrentUser {user_id:9002})).await;
            assert!(checked.errors.is_empty(),"{:?}",checked.errors);
            assert_eq!(checked.data.into_json()?["recheckPaymentOrder"]["status"],"credited");
            let client=reqwest::Client::new();
            let callback=format!("http://{callback_address}/api/payments/epay/notify?{raw}");
            let response=client.get(&callback).send().await?;
            assert!(response.status().is_success());assert_eq!(response.text().await?,"success");
            let response=client.get(&callback).send().await?;assert_eq!(response.text().await?,"success");
            assert_eq!(adapter.order(9002,project,order.id.as_str()).await?.status,"credited");
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT credit_balance_micros FROM project_wallets WHERE project_id=$1").bind(project).fetch_one(&pool).await?,100_000_000);
            assert_eq!(sqlx::query_scalar::<_,String>("SELECT provider_transaction_id FROM payment_orders WHERE id=$1").bind(id).fetch_one(&pool).await?,"gateway-txn-1");
            assert_eq!(client.post(format!("http://{callback_address}/api/payments/epay/notify")).send().await?.status(),reqwest::StatusCode::METHOD_NOT_ALLOWED);
            assert!(adapter.order(9001,project,order.id.as_str()).await.is_err());
            assert!(adapter.order(9003,project,order.id.as_str()).await.is_err());
            // The notification/query discrepancy is durable review with no credit.
            let second=adapter.create(9002,project,CreatePaymentOrderInput {amount_minor:"2".into(),channel:PaymentChannel::Wechat,idempotency_key:"gateway-two".into()}).await?;
            let second_id=Uuid::parse_str(second.id.as_str())?;
            Mock::given(method("GET")).and(path("/api.php")).and(query_param("out_trade_no",second_id.to_string())).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":1,"status":1,"pid":"7","out_trade_no":second_id.to_string(),"trade_no":"gateway-txn-2","type":"wxpay","money":"0.02","endtime":paid_time()}))).mount(&gateway_server).await;
            notice.insert("out_trade_no".into(),second_id.to_string());notice.insert("trade_no".into(),"gateway-txn-2".into());
            notice.insert("sign".into(),crate::epay_classic_md5::signature(&notice,"test-key"));
            let raw=url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&notice).finish();
            adapter.notify(&raw).await?;
            assert_eq!(adapter.order(9002,project,second.id.as_str()).await?.status,"review_required");
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup'").fetch_one(&pool).await?,1);
            // Exercise the production GraphQL permission gate AND the service
            // permit, including a selected Project that must not grant global access.
            use conduit_auth::{Principal, request_context::RequestContext, scopes::{Scope,slug}};
            use conduit_admin_graphql::payments::OperatorPaymentAccess;
            let context = |scopes:Vec<String>| {
                let mut principal=Principal::user("9003"); principal.scopes.extend(scopes);
                let mut request=RequestContext::new();request.set_principal(principal).unwrap();
                request.set_project_id(project.to_string()).unwrap();request
            };

            let read=format!("query {{ operatorPaymentOrders {{ payerUserID order {{ id checkoutURL }} }} operatorPaymentOrder(id: \"{}\") {{ payerUserID order {{ id }} }} }}",order.id.as_str());
            let recheck=|id:&str|format!("mutation {{ operatorRecheckPaymentOrder(id: \"{id}\") {{ payerUserID order {{ id status reviewReason checkoutURL }} }} }}");
            let ordinary=context(vec![]);
            let project_admin=context(vec![Scope::project_role(project.to_string(),"*").to_string(),Scope::project_membership(project.to_string(),slug::READ_BILLING).to_string()]);
            for request in [&ordinary,&project_admin] {
                assert!(OperatorPaymentAccess::from_request(request,false).is_err());
                assert!(OperatorPaymentAccess::from_request(request,true).is_err());
                assert!(!schema.execute(async_graphql::Request::new(&read).data(request.clone())).await.errors.is_empty());
                assert!(!schema.execute(async_graphql::Request::new(recheck(order.id.as_str())).data(request.clone())).await.errors.is_empty());
            }
            let reader=context(vec![slug::READ_BILLING.into()]);
            let writer=context(vec![slug::WRITE_BILLING.into()]);
            let role_reader=context(vec![Scope::system_role(slug::READ_BILLING).to_string()]);
            for request in [&reader,&role_reader] {
                let response=schema.execute(async_graphql::Request::new(&read).data(request.clone())).await;
                assert!(response.errors.is_empty(),"{:?}",response.errors);
                let data=response.data.into_json()?;
                assert_eq!(data["operatorPaymentOrder"]["payerUserID"],"9002");
                assert!(data["operatorPaymentOrders"][0]["order"]["checkoutURL"].is_null());
            }
            assert!(!schema.execute(async_graphql::Request::new(recheck(order.id.as_str())).data(reader.clone())).await.errors.is_empty());
            assert!(!schema.execute(async_graphql::Request::new(&read).data(writer.clone())).await.errors.is_empty());
            assert!(adapter.operator_recheck(OperatorPaymentAccess::from_request(&reader,false)?,order.id.as_str()).await.is_err());
            let mut operator_orders=vec![];
            for (key,txn) in [("operator-active","operator-txn-active"),("operator-deleted","operator-txn-deleted")] {
                let order=adapter.create(9002,project,CreatePaymentOrderInput{amount_minor:"1".into(),channel:PaymentChannel::Wechat,idempotency_key:key.into()}).await?;
                Mock::given(method("GET")).and(path("/api.php")).and(query_param("out_trade_no",order.id.as_str()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":1,"status":1,"pid":"7","out_trade_no":order.id.as_str(),"trade_no":txn,"type":"wxpay","money":"0.01","endtime":paid_time()}))).mount(&gateway_server).await;
                operator_orders.push(order);
            }
            let active=&operator_orders[0];
            for _ in 0..2 {
                let response=schema.execute(async_graphql::Request::new(recheck(active.id.as_str())).data(writer.clone())).await;
                assert!(response.errors.is_empty(),"{:?}",response.errors);
                assert_eq!(response.data.into_json()?["operatorRecheckPaymentOrder"]["order"]["status"],"credited");
            }
            sqlx::query("UPDATE projects SET deleted_at=1 WHERE id=$1").bind(project).execute(&pool).await?;
            assert!(adapter.order(9002,project,operator_orders[1].id.as_str()).await.is_err());
            // No current Project is required, even after the frozen target is deleted.
            let mut global_writer=writer.clone();global_writer.project_id=None;
            let mut global_reader=reader.clone();global_reader.project_id=None;
            for _ in 0..2 {
                let response=schema.execute(async_graphql::Request::new(recheck(operator_orders[1].id.as_str())).data(global_writer.clone())).await;
                assert!(response.errors.is_empty(),"{:?}",response.errors);
                assert_eq!(response.data.into_json()?["operatorRecheckPaymentOrder"]["order"]["status"],"review_required");
            }
            let response=schema.execute(async_graphql::Request::new(&read).data(global_reader)).await;
            assert!(response.errors.is_empty(),"{:?}",response.errors);
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup'").fetch_one(&pool).await?,2);
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT credit_balance_micros FROM project_wallets WHERE project_id=$1").bind(project).fetch_one(&pool).await?,200_000_000);
            Ok(())
        }.await;
        server.abort();
        let _ = server.await;
        pool.close().await;
        let cleanup = database.cleanup().await;
        result?;
        cleanup?;
        Ok(())
    }
    #[tokio::test]
    async fn postgres_topup_freeze_retry_concurrency_and_review()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pool = &database.pool;
            database.owners(&[9001]).await?;
            let project: i64 = sqlx::query_scalar("INSERT INTO projects(name,status,profiles) VALUES('Payment fixture','active','{}'::jsonb) RETURNING id").fetch_one(pool).await?;
            sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES(9001,$1,true,'[]'::jsonb)").bind(project).execute(pool).await?;
            sqlx::query("INSERT INTO user_groups(id,name,status,recharge_multiplier_ppm) VALUES('g','Topup fixture','enabled',1500000)").execute(pool).await?;
            sqlx::query("INSERT INTO user_group_users(user_id,user_group_id) VALUES(9001,'g')").execute(pool).await?;
            let make_input = |key: &str, amount| NewPaymentOrder { payer:9001, project, amount_minor:amount, channel:"alipay".into(), idempotency_key:key.into(), merchant_identity:"fixture-config-v1".into(), provider_config:"{}".into(), expires_at:Utc::now()+chrono::Duration::minutes(10) };
            let id = create_local_order(pool,make_input("one",1)).await?;
            assert_eq!(id,create_local_order(pool,make_input("one",1)).await?);
            assert!(create_local_order(pool,make_input("one",2)).await.is_err());
            let mut creation_tasks=vec![];
            for _ in 0..4 {let pool=pool.clone();let input=make_input("concurrent-creation",1);creation_tasks.push(tokio::spawn(async move {create_local_order(&pool,input).await}));}
            let mut created=std::collections::BTreeSet::new();
            for task in creation_tasks {created.insert(task.await??);}
            assert_eq!(created.len(),1);
            use conduit_admin_graphql::groups::{GroupServices, SaveUserGroupInput, GroupStatus};
            let groups=crate::wiring_postgres_groups::PgGroupAdapter::new(pool.clone());
            let group_input=|ppm:Option<&str>,revision:Option<&str>| SaveUserGroupInput {id:Some(ID("g".into())),name:"Topup fixture".into(),description:None,status:GroupStatus::Enabled,recharge_multiplier_ppm:ppm.map(str::to_owned),expected_recharge_revision:revision.map(str::to_owned),member_user_ids:vec![ID("9001".into())],model_group_ids:vec![]};
            let preserved=groups.save_user_group(group_input(None,None)).await?;
            assert_eq!(preserved.recharge_multiplier_ppm,"1500000");
            assert_eq!(preserved.recharge_revision,"1");
            let changed=groups.save_user_group(group_input(Some("2000000"),Some("1"))).await?;
            assert_eq!(changed.recharge_revision,"2");
            assert!(groups.save_user_group(group_input(Some("3000000"),Some("1"))).await.is_err());
            assert_eq!(groups.user_groups().await?.into_iter().find(|g|g.id.as_str()=="g").unwrap().recharge_multiplier_ppm,"2000000");
            let paid = VerifiedPaid {order_id:id,merchant_identity:"fixture-config-v1".into(),transaction_id:"fixture-paid-1".into(),channel:"alipay".into(),currency:"CNY".into(),amount_minor:1,paid_at:Utc::now(),verification_mismatch:false,verification_context:serde_json::json!({})};
            // Force the existing wallet trigger to fail; no ledger/order/audit
            // outcome may commit, and the same verified payment must recover.
            sqlx::query("INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at,credit_balance_micros) VALUES($1,'STATION_CREDIT','active',now(),now(),$2)").bind(project).bind(i64::MAX).execute(pool).await?;
            assert!(apply_verified_paid(pool,paid.clone()).await.is_err());
            assert_eq!(sqlx::query_scalar::<_,String>("SELECT status FROM payment_orders WHERE id=$1").bind(id).fetch_one(pool).await?,"pending");
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup'").fetch_one(pool).await?,0);
            sqlx::query("UPDATE project_wallets SET credit_balance_micros=0 WHERE project_id=$1").bind(project).execute(pool).await?;
            let mut tasks = vec![];
            for _ in 0..8 { let pool=pool.clone(); let paid=paid.clone(); tasks.push(tokio::spawn(async move { apply_verified_paid(&pool,paid).await })); }
            for task in tasks { assert_eq!(task.await??,"credited"); }
            let balance: i64 = sqlx::query_scalar("SELECT credit_balance_micros FROM project_wallets WHERE project_id=$1").bind(project).fetch_one(pool).await?;
            assert_eq!(balance,150_000_000);
            use conduit_admin_graphql::billing::BillingServices;
            let visible=crate::wiring_postgres_billing::PgBillingAdapter::new(pool.clone()).project_balance(&project.to_string()).await?;
            let topup_entry=visible.ledger_entries.iter().find(|e|e.entry_type=="payment_topup").ok_or("payment topup missing from Project balance read model")?;
            assert_eq!(topup_entry.amount.parse::<Decimal>()?,Decimal::from(150));
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup'").fetch_one(pool).await?,1);
            let adapter=PgPaymentAdapter::new(pool.clone());
            assert_eq!(adapter.order(9001,project,&id.to_string()).await?.recharge_multiplier_ppm,"1500000");
            let reused = create_local_order(pool,make_input("two",1)).await?;
            let mut second=paid.clone(); second.order_id=reused;
            assert_eq!(apply_verified_paid(pool,second).await?,"review_required");
            let mismatch = create_local_order(pool,make_input("three",1)).await?;
            let mut wrong=paid.clone(); wrong.order_id=mismatch; wrong.transaction_id="fixture-paid-wrong".into(); wrong.amount_minor=2;
            assert_eq!(apply_verified_paid(pool,wrong).await?,"review_required");
            let same_txn_order=create_local_order(pool,make_input("mismatched-txn-reuse",2)).await?;
            let mut same_txn=paid.clone();same_txn.order_id=same_txn_order;same_txn.transaction_id="fixture-paid-wrong".into();same_txn.amount_minor=2;
            assert_eq!(apply_verified_paid(pool,same_txn).await?,"review_required");
            assert_eq!(sqlx::query_scalar::<_,String>("SELECT review_reason FROM payment_orders WHERE id=$1").bind(same_txn_order).fetch_one(pool).await?,"transaction_reused");
            assert!(sqlx::query("UPDATE payment_orders SET provider_transaction_id='replacement' WHERE id=$1").bind(mismatch).execute(pool).await.is_err());
            let late = create_local_order(pool,make_input("four",1)).await?;
            let mut late_paid=paid.clone(); late_paid.order_id=late; late_paid.transaction_id="fixture-late".into(); late_paid.paid_at=Utc::now()+chrono::Duration::hours(1);
            assert_eq!(apply_verified_paid(pool,late_paid).await?,"review_required");
            let currency = create_local_order(pool,make_input("five",1)).await?;
            let mut wrong_currency=paid.clone();wrong_currency.order_id=currency;wrong_currency.transaction_id="fixture-currency".into();wrong_currency.currency="USD".into();
            assert_eq!(apply_verified_paid(pool,wrong_currency).await?,"review_required");
            let closed = create_local_order(pool,make_input("six",1)).await?;
            sqlx::query("UPDATE project_wallets SET status='closed' WHERE project_id=$1").bind(project).execute(pool).await?;
            let mut closed_paid=paid.clone();closed_paid.order_id=closed;closed_paid.transaction_id="fixture-closed-wallet".into();
            assert_eq!(apply_verified_paid(pool,closed_paid).await?,"review_required");
            sqlx::query("UPDATE project_wallets SET status='active' WHERE project_id=$1").bind(project).execute(pool).await?;
            let deleted = create_local_order(pool,make_input("seven",1)).await?;
            sqlx::query("UPDATE projects SET deleted_at=1 WHERE id=$1").bind(project).execute(pool).await?;
            let mut deleted_paid=paid.clone();deleted_paid.order_id=deleted;deleted_paid.transaction_id="fixture-deleted-project".into();
            assert_eq!(apply_verified_paid(pool,deleted_paid).await?,"review_required");
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM project_credit_ledger_entries WHERE entry_type='payment_topup'").fetch_one(pool).await?,1);
            assert!(sqlx::query("UPDATE payment_orders SET status='closed' WHERE id=$1").bind(id).execute(pool).await.is_err());
            assert!(sqlx::query("UPDATE payment_order_events SET outcome='closed' WHERE order_id=$1").bind(id).execute(pool).await.is_err());
            Ok(())
        }.await;
        let cleanup = database.cleanup().await;
        result?;
        cleanup?;
        Ok(())
    }
    #[test]
    fn uses_cny_quote_and_floors_final_credit_micros() {
        use conduit_core::objects::money::CurrencyExchangeRate;
        let mut settings = AccountingSettings {
            accounting_currency: "USD".into(),
            credits_per_accounting_unit: Decimal::ONE,
            ..AccountingSettings::default()
        };
        assert!(freeze_topup(settings.clone(), 1, 1_000_000, None, None).is_err());
        settings.exchange_rates.push(CurrencyExchangeRate {
            currency: "CNY".into(),
            quote_per_accounting_unit: Decimal::from(3),
        });
        let frozen =
            freeze_topup(settings.clone(), 1, 1_000_000, Some("g".into()), Some(4)).unwrap();
        assert_eq!(frozen.credits_micros, 3333);
        settings.credits_per_accounting_unit = Decimal::from(999);
        assert_eq!(frozen.accounting.credits_per_accounting_unit, Decimal::ONE);
        assert_eq!(frozen.group_revision, Some(4));
        assert!(freeze_topup(settings, 1, 0, None, None).is_err());
    }
    #[test]
    fn freezes_group_bonus_and_rejects_unsafe_amounts() {
        let f = freeze_topup(AccountingSettings::default(), 1, 1_500_000, None, None).unwrap();
        assert_eq!(f.credits_micros, 150_000_000);
        for raw in [
            "0",
            "01",
            "-1",
            "+1",
            "1.0",
            "1e2",
            " 1",
            "9223372036854775808",
        ] {
            assert!(parse_minor(raw).is_err(), "{raw}");
        }
        assert!(
            freeze_topup(
                AccountingSettings::default(),
                i64::MAX,
                MAX_RECHARGE_MULTIPLIER_PPM,
                None,
                None
            )
            .is_err()
        );
    }
}

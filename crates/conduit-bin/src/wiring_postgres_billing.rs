//! PostgreSQL implementation of the billing administration boundary.
//!
//! Project wallets are the only spendable credit ledger. The user-scoped
//! credit tables are exposed only through the legacy read API required by the
//! GraphQL contract; new credit must be granted to a concrete Project.

use std::{collections::BTreeSet, str::FromStr};

use async_graphql::ID;
use chrono::{DateTime, Duration, Months, Utc};
use conduit_admin_graphql::billing as gql;
use conduit_core::objects::money::STATION_CREDIT_CODE;
use conduit_services::billing::{decimal_to_micros, micros_to_decimal};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccessVersionSnapshot {
    access_plan_id: i64,
    access_plan_version_id: i64,
    name: String,
}

#[derive(Debug, Clone)]
struct ValidatedQuotaRule {
    id: Option<i64>,
    name: String,
    quota_class: gql::QuotaClass,
    amount_micros: i64,
    rollover_mode: gql::RolloverMode,
    rollover_cap_micros: Option<i64>,
    carry_duration_seconds: Option<i64>,
    access_plan_ids: Vec<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct PgBillingAdapter {
    pool: PgPool,
}

impl PgBillingAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn begin_read_snapshot(&self) -> Result<Transaction<'_, Postgres>, gql::BillingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        Ok(tx)
    }

    async fn project_balance_value(
        &self,
        project_id: i64,
    ) -> Result<gql::ProjectBalance, gql::BillingError> {
        let mut tx = self.begin_read_snapshot().await?;
        let balance = self.project_balance_value_tx(&mut tx, project_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(balance)
    }

    async fn project_balance_value_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        project_id: i64,
    ) -> Result<gql::ProjectBalance, gql::BillingError> {
        ensure_project_tx(tx, project_id).await?;
        let read_at = read_snapshot_time(tx).await?;
        let wallet = sqlx::query(
            "SELECT id,status,credit_balance_micros,credit_reserved_micros FROM project_wallets \
             WHERE project_id=$1 AND currency=$2 ORDER BY id LIMIT 1",
        )
        .bind(project_id)
        .bind(STATION_CREDIT_CODE)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?;
        let (wallet_id, wallet_status, credit, credit_reserved) = wallet
            .map(|row| {
                (
                    Some(row.get::<i64, _>("id")),
                    row.get::<String, _>("status"),
                    row.get::<i64, _>("credit_balance_micros"),
                    row.get::<i64, _>("credit_reserved_micros"),
                )
            })
            .unwrap_or((None, "uninitialized".into(), 0, 0));
        let bucket = sqlx::query(
            "SELECT COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2),0)::BIGINT AS remaining, \
                    COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2 AND b.quota_class='GENERAL'),0)::BIGINT AS general_remaining, \
                    COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2 AND b.quota_class='DEDICATED'),0)::BIGINT AS dedicated_remaining, \
                    COALESCE(SUM(b.reserved_micros) FILTER (WHERE b.status IN ('active','draining','paused')),0)::BIGINT AS reserved \
             FROM subscription_allowance_buckets b \
             JOIN user_subscriptions s ON s.id=b.subscription_id \
             JOIN user_subscription_projects sp ON sp.subscription_id=s.id \
             WHERE sp.project_id=$1 AND s.status IN ('active','cancel_pending','paused')",
        )
        .bind(project_id)
        .bind(read_at)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?;
        let subscription = bucket.get::<i64, _>("remaining").max(0);
        let general_subscription = bucket.get::<i64, _>("general_remaining").max(0);
        let dedicated_subscription = bucket.get::<i64, _>("dedicated_remaining").max(0);
        let subscription_reserved = bucket.get::<i64, _>("reserved").max(0);
        let entries = if let Some(wallet_id) = wallet_id {
            sqlx::query(
                "SELECT id,amount_micros,entry_type,description,created_at \
                 FROM project_credit_ledger_entries WHERE wallet_id=$1 \
                 ORDER BY id DESC LIMIT 50",
            )
            .bind(wallet_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(storage)?
            .into_iter()
            .map(|row| gql::CreditLedgerEntry {
                id: id(row.get("id")),
                amount: amount(row.get("amount_micros")),
                entry_type: row.get("entry_type"),
                description: row.get("description"),
                created_at: wire_time(row.get("created_at")),
            })
            .collect()
        } else {
            Vec::new()
        };
        let available_credit = (credit - credit_reserved).max(0);
        Ok(gql::ProjectBalance {
            project_id: project_node_id(project_id),
            currency: STATION_CREDIT_CODE.into(),
            wallet_status,
            credit_balance: amount(credit),
            subscription_balance: amount(subscription),
            general_subscription_balance: amount(general_subscription),
            dedicated_subscription_balance: amount(dedicated_subscription),
            reserved_balance: amount(credit_reserved + subscription_reserved),
            available_balance: amount(available_credit + subscription),
            ledger_entries: entries,
        })
    }

    async fn user_balance_value(
        &self,
        user_id: i64,
    ) -> Result<gql::UserBalance, gql::BillingError> {
        let mut tx = self.begin_read_snapshot().await?;
        let balance = self.user_balance_value_tx(&mut tx, user_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(balance)
    }

    async fn user_balance_value_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: i64,
    ) -> Result<gql::UserBalance, gql::BillingError> {
        ensure_user_tx(tx, user_id).await?;
        let read_at = read_snapshot_time(tx).await?;
        let account = sqlx::query(
            "SELECT id FROM credit_accounts \
             WHERE user_id=$1 AND currency=$2 AND status='enabled' ORDER BY id LIMIT 1",
        )
        .bind(user_id)
        .bind(STATION_CREDIT_CODE)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?;
        let account_id = account.map(|row| row.get::<i64, _>("id"));
        let credit = if let Some(account_id) = account_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT \
                 FROM credit_ledger_entries WHERE account_id=$1",
            )
            .bind(account_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(storage)?
        } else {
            0
        };
        let credit_reserved = if let Some(account_id) = account_id {
            sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(SUM(amount_micros),0)::BIGINT FROM credit_reservations \
                 WHERE account_id=$1 AND status='reserved' AND expires_at>$2",
            )
            .bind(account_id)
            .bind(read_at)
            .fetch_one(&mut **tx)
            .await
            .map_err(storage)?
        } else {
            0
        };
        let bucket = sqlx::query(
            "SELECT COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2),0)::BIGINT AS remaining, \
                    COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2 AND b.quota_class='GENERAL'),0)::BIGINT AS general_remaining, \
                    COALESCE(SUM(GREATEST(b.granted_micros-b.consumed_micros-b.reserved_micros,0)) \
                              FILTER (WHERE b.status='active' AND b.expires_at>$2 AND b.quota_class='DEDICATED'),0)::BIGINT AS dedicated_remaining, \
                    COALESCE(SUM(b.reserved_micros) FILTER (WHERE b.status IN ('active','draining','paused')),0)::BIGINT AS reserved \
             FROM subscription_allowance_buckets b \
             JOIN user_subscriptions s ON s.id=b.subscription_id \
             WHERE s.user_id=$1 AND s.status IN ('active','cancel_pending','paused')",
        )
        .bind(user_id)
        .bind(read_at)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?;
        let subscription = bucket.get::<i64, _>("remaining").max(0);
        let general_subscription = bucket.get::<i64, _>("general_remaining").max(0);
        let dedicated_subscription = bucket.get::<i64, _>("dedicated_remaining").max(0);
        let subscription_reserved = bucket.get::<i64, _>("reserved").max(0);
        let entries = if let Some(account_id) = account_id {
            sqlx::query(
                "SELECT id,amount_micros,entry_type,description,created_at \
                 FROM credit_ledger_entries WHERE account_id=$1 ORDER BY id DESC LIMIT 50",
            )
            .bind(account_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(storage)?
            .into_iter()
            .map(|row| gql::CreditLedgerEntry {
                id: id(row.get("id")),
                amount: amount(row.get("amount_micros")),
                entry_type: row.get("entry_type"),
                description: row.get("description"),
                created_at: wire_time(row.get("created_at")),
            })
            .collect()
        } else {
            Vec::new()
        };
        Ok(gql::UserBalance {
            user_id: user_node_id(user_id),
            currency: STATION_CREDIT_CODE.into(),
            credit_balance: amount(credit),
            subscription_balance: amount(subscription),
            general_subscription_balance: amount(general_subscription),
            dedicated_subscription_balance: amount(dedicated_subscription),
            reserved_balance: amount(credit_reserved + subscription_reserved),
            available_balance: amount((credit - credit_reserved).max(0) + subscription),
            ledger_entries: entries,
        })
    }

    async fn plan(&self, plan_id: i64) -> Result<gql::SubscriptionPlan, gql::BillingError> {
        let row = sqlx::query(
            "SELECT id,name,currency,interval_unit,interval_count,status \
             FROM subscription_plans WHERE id=$1",
        )
        .bind(plan_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::BillingError::NotFound(format!("subscription plan {plan_id}")))?;
        let access_plans = sqlx::query(
            "SELECT a.id,a.name FROM subscription_plan_access_plans s \
             JOIN access_plans a ON a.id=s.access_plan_id \
             WHERE s.subscription_plan_id=$1 ORDER BY LOWER(a.name),a.id",
        )
        .bind(plan_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(|row| gql::SubscriptionAccessPlan {
            id: id(row.get("id")),
            name: row.get("name"),
        })
        .collect();
        let quota_rules = load_plan_quota_rules(&self.pool, plan_id).await?;
        Ok(plan_from_row(row, access_plans, quota_rules))
    }

    async fn subscription_plan_snapshot_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        subscription_id: i64,
    ) -> Result<gql::SubscriptionPlan, gql::BillingError> {
        let row = sqlx::query(
            "SELECT p.id,p.name,p.currency,s.assigned_interval_unit AS interval_unit, \
                    s.assigned_interval_count AS interval_count,p.status \
             FROM user_subscriptions s JOIN subscription_plans p ON p.id=s.plan_id \
             WHERE s.id=$1",
        )
        .bind(subscription_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
        let access_plans = sqlx::query(
            "SELECT a.id,a.name FROM user_subscription_access_plan_snapshots s \
             JOIN access_plans a ON a.id=s.access_plan_id \
             WHERE s.subscription_id=$1 ORDER BY LOWER(a.name),a.id",
        )
        .bind(subscription_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?
        .into_iter()
        .map(|row| gql::SubscriptionAccessPlan {
            id: id(row.get("id")),
            name: row.get("name"),
        })
        .collect();
        let quota_rules = load_subscription_quota_rule_snapshots_tx(tx, subscription_id).await?;
        Ok(plan_from_row(row, access_plans, quota_rules))
    }

    async fn subscription(
        &self,
        subscription_id: i64,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let mut tx = self.begin_read_snapshot().await?;
        let subscription = self.subscription_tx(&mut tx, subscription_id).await?;
        tx.commit().await.map_err(storage)?;
        Ok(subscription)
    }

    async fn subscription_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        subscription_id: i64,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let read_at = read_snapshot_time(tx).await?;
        let row = sqlx::query(
            "SELECT s.user_id,s.status,s.current_period_start,s.current_period_end,s.auto_renew, \
                    s.assigned_interval_unit,s.assigned_interval_count \
             FROM user_subscriptions s WHERE s.id=$1",
        )
        .bind(subscription_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
        let plan = self
            .subscription_plan_snapshot_tx(tx, subscription_id)
            .await?;
        let bucket_rows = sqlx::query(
            "SELECT b.id,b.quota_rule_snapshot_id,b.entitlement_snapshot_id,b.quota_class, \
                    b.period_start,b.period_end,b.expires_at,b.granted_micros,b.consumed_micros, \
                    b.reserved_micros,b.status,b.source_bucket_id,r.rule_name,r.access_plan_versions \
             FROM subscription_allowance_buckets b \
             JOIN user_subscription_quota_rule_snapshots r ON r.id=b.quota_rule_snapshot_id \
             WHERE b.subscription_id=$1 AND b.status IN ('active','draining','paused') \
             ORDER BY b.expires_at,b.id",
        )
        .bind(subscription_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
        let mut allowance_buckets = Vec::with_capacity(bucket_rows.len());
        let mut granted = 0_i64;
        let mut consumed = 0_i64;
        let mut reserved = 0_i64;
        let mut general_remaining = 0_i64;
        let mut dedicated_remaining = 0_i64;
        for bucket in bucket_rows {
            let granted_micros: i64 = bucket.get("granted_micros");
            let consumed_micros: i64 = bucket.get("consumed_micros");
            let reserved_micros: i64 = bucket.get("reserved_micros");
            let bucket_status: String = bucket.get("status");
            let expires_at: DateTime<Utc> = bucket.get("expires_at");
            let raw_remaining = (granted_micros - consumed_micros - reserved_micros).max(0);
            let is_current =
                matches!(bucket_status.as_str(), "active" | "paused") && expires_at > read_at;
            let remaining = if is_current { raw_remaining } else { 0 };
            let effective_status = if expires_at <= read_at {
                if reserved_micros > 0 {
                    "draining"
                } else {
                    "expired"
                }
            } else {
                bucket_status.as_str()
            };
            if is_current {
                granted = granted.saturating_add(granted_micros);
                consumed = consumed.saturating_add(consumed_micros);
                reserved = reserved.saturating_add(reserved_micros);
            }
            let quota_class =
                quota_class_from_wire(bucket.get::<String, _>("quota_class").as_str());
            if quota_class == gql::QuotaClass::General {
                general_remaining = general_remaining.saturating_add(remaining);
            } else {
                dedicated_remaining = dedicated_remaining.saturating_add(remaining);
            }
            let access_versions =
                parse_access_version_snapshot(bucket.get::<Value, _>("access_plan_versions"))?;
            let access_plans = access_versions
                .iter()
                .map(|snapshot| gql::SubscriptionAccessPlan {
                    id: id(snapshot.access_plan_id),
                    name: snapshot.name.clone(),
                })
                .collect();
            let model_ids = sqlx::query_scalar::<_, String>(
                "SELECT m.model_id FROM subscription_entitlement_snapshot_items i \
                 JOIN models m ON m.id=i.public_model_id \
                 WHERE i.snapshot_id=$1 AND i.quota_rule_snapshot_id=$2 \
                 ORDER BY LOWER(m.model_id),m.id",
            )
            .bind(bucket.get::<i64, _>("entitlement_snapshot_id"))
            .bind(bucket.get::<i64, _>("quota_rule_snapshot_id"))
            .fetch_all(&mut **tx)
            .await
            .map_err(storage)?;
            let source_bucket_id = bucket
                .try_get::<Option<i64>, _>("source_bucket_id")
                .ok()
                .flatten();
            allowance_buckets.push(gql::SubscriptionAllowanceBucket {
                id: id(bucket.get("id")),
                name: bucket.get("rule_name"),
                quota_class,
                source_type: if source_bucket_id.is_some() {
                    "CARRYOVER".into()
                } else {
                    "CURRENT".into()
                },
                period_start: wire_time(bucket.get("period_start")),
                period_end: wire_time(bucket.get("period_end")),
                expires_at: wire_time(expires_at),
                granted_allowance: amount(granted_micros),
                consumed_allowance: amount(consumed_micros),
                reserved_allowance: amount(reserved_micros),
                remaining_allowance: amount(remaining),
                status: effective_status.into(),
                access_plans,
                model_ids,
                source_bucket_id: source_bucket_id.map(id),
            });
        }
        let project_id = sqlx::query_scalar::<_, i64>(
            "SELECT project_id FROM user_subscription_projects WHERE subscription_id=$1",
        )
        .bind(subscription_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?;

        let grant_rows = sqlx::query(
            "SELECT p.id,p.name,v.id AS version_id FROM project_access_grants g \
             JOIN access_plan_versions v ON v.id=g.access_plan_version_id \
             JOIN access_plans p ON p.id=v.access_plan_id \
             WHERE g.source_type='subscription' AND g.source_id LIKE $1 \
               AND g.status='active' ORDER BY LOWER(p.name),p.id",
        )
        .bind(format!(
            "{}%",
            subscription_grant_source_prefix(subscription_id)
        ))
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
        let mut granted_access_plans = Vec::with_capacity(grant_rows.len());
        let mut granted_group_names = BTreeSet::new();
        let mut granted_model_ids = BTreeSet::new();
        for grant in grant_rows {
            let access_plan_id: i64 = grant.get("id");
            let version_id: i64 = grant.get("version_id");
            granted_access_plans.push(gql::SubscriptionAccessPlan {
                id: id(access_plan_id),
                name: grant.get("name"),
            });
            granted_group_names.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT name FROM simple_groups WHERE access_plan_id=$1 \
                     AND status<>'archived' ORDER BY LOWER(name),id",
                )
                .bind(access_plan_id)
                .fetch_all(&mut **tx)
                .await
                .map_err(storage)?,
            );
            granted_model_ids.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT m.model_id FROM access_plan_items i \
                     JOIN models m ON m.id=i.public_model_id AND m.deleted_at=0 \
                     WHERE i.access_plan_version_id=$1 ORDER BY LOWER(m.model_id),m.id",
                )
                .bind(version_id)
                .fetch_all(&mut **tx)
                .await
                .map_err(storage)?,
            );
        }
        Ok(gql::UserSubscription {
            id: id(subscription_id),
            user_id: user_node_id(row.get("user_id")),
            interval_unit: interval_from_wire(
                row.get::<String, _>("assigned_interval_unit").as_str(),
            ),
            interval_count: row.get("assigned_interval_count"),
            plan,
            status: row.get("status"),
            current_period_start: wire_time(row.get("current_period_start")),
            current_period_end: wire_time(row.get("current_period_end")),
            auto_renew: row.get("auto_renew"),
            project_id: project_id.map(project_node_id),
            granted_access_plans,
            granted_group_names: granted_group_names.into_iter().collect(),
            granted_model_ids: granted_model_ids.into_iter().collect(),
            granted_allowance: amount(granted),
            consumed_allowance: amount(consumed),
            reserved_allowance: amount(reserved),
            remaining_allowance: amount(general_remaining.saturating_add(dedicated_remaining)),
            allowance_buckets,
            general_remaining_allowance: amount(general_remaining),
            dedicated_remaining_allowance: amount(dedicated_remaining),
        })
    }

    async fn refresh(
        &self,
        subscription_id: i64,
        _force_initial: bool,
    ) -> Result<(), gql::BillingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        refresh_locked(&mut tx, subscription_id).await?;
        tx.commit().await.map_err(storage)
    }

    async fn set_inactive_status(
        &self,
        subscription_id: i64,
        allowed_statuses: &[&str],
        target_status: &str,
        disable_auto_renew: bool,
    ) -> Result<(), gql::BillingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        set_inactive_locked(
            &mut tx,
            subscription_id,
            allowed_statuses,
            target_status,
            disable_auto_renew,
        )
        .await?;
        tx.commit().await.map_err(storage)
    }

    async fn reactivate_subscription(
        &self,
        subscription_id: i64,
        source_status: &str,
        reset_period: bool,
    ) -> Result<(), gql::BillingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let row = sqlx::query(
            "SELECT s.plan_id,s.status,s.current_period_start,s.current_period_end, \
                    s.assigned_interval_unit AS interval_unit, \
                    s.assigned_interval_count AS interval_count,p.status AS plan_status \
             FROM user_subscriptions s JOIN subscription_plans p ON p.id=s.plan_id \
             WHERE s.id=$1 FOR UPDATE OF s",
        )
        .bind(subscription_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
        if row.get::<String, _>("plan_status") != "enabled" {
            return Err(gql::BillingError::Invalid(
                "subscription plan is not enabled".into(),
            ));
        }
        let current: String = row.get("status");
        if current == "active" {
            tx.commit().await.map_err(storage)?;
            return Ok(());
        }
        if current != source_status {
            return Err(gql::BillingError::Invalid(format!(
                "subscription cannot transition from {current} to active"
            )));
        }

        let now = Utc::now();
        let old_start: DateTime<Utc> = row.get("current_period_start");
        let old_end: DateTime<Utc> = row.get("current_period_end");
        let needs_new_period = reset_period || old_end <= now;
        let (start, end) = if needs_new_period {
            let interval_count = positive_interval_count(row.get("interval_count"))?;
            (
                now,
                next_period(
                    now,
                    row.get::<String, _>("interval_unit").as_str(),
                    interval_count,
                )?,
            )
        } else {
            (old_start, old_end)
        };
        lock_live_model_group_versions(&mut tx).await?;
        let access_versions = subscription_access_versions(&mut tx, subscription_id).await?;
        sync_subscription_access_grants(
            &mut tx,
            subscription_id,
            &access_versions,
            start,
            end,
            now,
        )
        .await?;
        if needs_new_period {
            issue_period_buckets(
                &mut tx,
                subscription_id,
                start,
                end,
                "active",
                if source_status == "paused" && !reset_period {
                    Some(old_start)
                } else {
                    None
                },
                now,
            )
            .await?;
        } else {
            sqlx::query(
                "UPDATE subscription_allowance_buckets SET status='active',updated_at=$1 \
                 WHERE subscription_id=$2 AND status=$3 AND expires_at>$1",
            )
            .bind(now)
            .bind(subscription_id)
            .bind(source_status)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            sqlx::query(
                "UPDATE subscription_allowance_buckets SET \
                 status=CASE WHEN reserved_micros>0 THEN 'draining' ELSE 'expired' END,updated_at=$1 \
                 WHERE subscription_id=$2 AND status=$3 AND expires_at<=$1",
            )
            .bind(now)
            .bind(subscription_id)
            .bind(source_status)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        sqlx::query(
            "UPDATE user_subscriptions SET status='active',current_period_start=$1, \
             current_period_end=$2,updated_at=$3 WHERE id=$4",
        )
        .bind(start)
        .bind(end)
        .bind(now)
        .bind(subscription_id)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)
    }

    /// Process expired periods safely across multiple application instances.
    /// `SKIP LOCKED` ensures a due subscription is owned by one worker at a
    /// time, while the complete lifecycle transition remains in one transaction.
    pub(crate) async fn process_due_subscriptions(&self) -> Result<usize, gql::BillingError> {
        let mut processed = 0;
        loop {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let due = sqlx::query(
                "SELECT s.id,s.status,s.auto_renew,p.status AS plan_status \
                 FROM user_subscriptions s JOIN subscription_plans p ON p.id=s.plan_id \
                 WHERE s.status IN ('active','paused','cancel_pending') AND s.current_period_end<=now() \
                 ORDER BY s.id FOR UPDATE OF s SKIP LOCKED LIMIT 1",
            )
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            let Some(row) = due else {
                tx.commit().await.map_err(storage)?;
                break;
            };
            let subscription_id: i64 = row.get("id");
            let status: String = row.get("status");
            if matches!(status.as_str(), "active" | "paused")
                && row.get::<bool, _>("auto_renew")
                && row.get::<String, _>("plan_status") == "enabled"
            {
                refresh_locked(&mut tx, subscription_id).await?;
            } else {
                set_inactive_locked(
                    &mut tx,
                    subscription_id,
                    &["active", "paused", "cancel_pending"],
                    "expired",
                    true,
                )
                .await?;
            }
            tx.commit().await.map_err(storage)?;
            processed += 1;
        }
        Ok(processed)
    }
}

async fn load_plan_quota_rules(
    pool: &PgPool,
    plan_id: i64,
) -> Result<Vec<gql::SubscriptionQuotaRule>, gql::BillingError> {
    let rows = sqlx::query(
        "SELECT id,name,quota_class,amount_micros,rollover_mode,rollover_cap_micros, \
                carry_duration_seconds \
         FROM subscription_quota_rules WHERE subscription_plan_id=$1 \
         ORDER BY CASE WHEN quota_class='DEDICATED' THEN 0 ELSE 1 END,id",
    )
    .bind(plan_id)
    .fetch_all(pool)
    .await
    .map_err(storage)?;
    let mut rules = Vec::with_capacity(rows.len());
    for row in rows {
        let rule_id: i64 = row.get("id");
        let access_plans = sqlx::query(
            "SELECT p.id,p.name FROM subscription_quota_rule_access_plans r \
             JOIN access_plans p ON p.id=r.access_plan_id WHERE r.quota_rule_id=$1 \
             ORDER BY LOWER(p.name),p.id",
        )
        .bind(rule_id)
        .fetch_all(pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(|access_plan| gql::SubscriptionAccessPlan {
            id: id(access_plan.get("id")),
            name: access_plan.get("name"),
        })
        .collect();
        rules.push(quota_rule_from_row(row, access_plans));
    }
    Ok(rules)
}

async fn load_subscription_quota_rule_snapshots_tx(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
) -> Result<Vec<gql::SubscriptionQuotaRule>, gql::BillingError> {
    let rows = sqlx::query(
        "SELECT id,rule_name AS name,quota_class,amount_micros,rollover_mode, \
                rollover_cap_micros,carry_duration_seconds,access_plan_versions \
         FROM user_subscription_quota_rule_snapshots WHERE subscription_id=$1 \
         ORDER BY CASE WHEN quota_class='DEDICATED' THEN 0 ELSE 1 END,id",
    )
    .bind(subscription_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?;
    rows.into_iter()
        .map(|row| {
            let access_plans =
                parse_access_version_snapshot(row.get::<Value, _>("access_plan_versions"))?
                    .into_iter()
                    .map(|snapshot| gql::SubscriptionAccessPlan {
                        id: id(snapshot.access_plan_id),
                        name: snapshot.name,
                    })
                    .collect();
            Ok(quota_rule_from_row(row, access_plans))
        })
        .collect()
}

fn validate_quota_rules(
    inputs: Vec<gql::SubscriptionQuotaRuleInput>,
) -> Result<Vec<ValidatedQuotaRule>, gql::BillingError> {
    if inputs.is_empty() {
        return Err(gql::BillingError::Invalid(
            "quotaRules must contain at least one rule".into(),
        ));
    }
    inputs
        .into_iter()
        .enumerate()
        .map(|(index, input)| {
            let name = nonempty(input.name, &format!("quotaRules[{index}].name"))?;
            let amount_micros = parse_positive_amount(&input.allowance)?;
            let access_plan_ids = parse_access_plan_ids(input.access_plan_ids)?;
            match input.quota_class {
                gql::QuotaClass::General if !access_plan_ids.is_empty() => {
                    return Err(gql::BillingError::Invalid(format!(
                        "general quota rule {name:?} cannot have access plans"
                    )));
                }
                gql::QuotaClass::Dedicated if access_plan_ids.is_empty() => {
                    return Err(gql::BillingError::Invalid(format!(
                        "dedicated quota rule {name:?} requires at least one access plan"
                    )));
                }
                _ => {}
            }
            let rollover_mode = input.rollover_mode.unwrap_or(gql::RolloverMode::None);
            let (rollover_cap_micros, carry_duration_seconds) = match rollover_mode {
                gql::RolloverMode::None => {
                    if input.rollover_cap.is_some() || input.carryover_days.is_some() {
                        return Err(gql::BillingError::Invalid(format!(
                            "quota rule {name:?} can only set rolloverCap and carryoverDays when rolloverMode is CAPPED"
                        )));
                    }
                    (None, None)
                }
                gql::RolloverMode::Capped => {
                    let cap = input.rollover_cap.as_deref().ok_or_else(|| {
                        gql::BillingError::Invalid(format!(
                            "quota rule {name:?} requires rolloverCap"
                        ))
                    })?;
                    let cap = parse_positive_amount(cap)?;
                    let days = input.carryover_days.ok_or_else(|| {
                        gql::BillingError::Invalid(format!(
                            "quota rule {name:?} requires carryoverDays"
                        ))
                    })?;
                    if !(1..=3650).contains(&days) {
                        return Err(gql::BillingError::Invalid(format!(
                            "quota rule {name:?} carryoverDays must be between 1 and 3650"
                        )));
                    }
                    (
                        Some(cap),
                        Some(i64::from(days).saturating_mul(SECONDS_PER_DAY)),
                    )
                }
            };
            Ok(ValidatedQuotaRule {
                id: input.id.as_ref().map(|value| parse_id(value.as_str())).transpose()?,
                name,
                quota_class: input.quota_class,
                amount_micros,
                rollover_mode,
                rollover_cap_micros,
                carry_duration_seconds,
                access_plan_ids,
            })
        })
        .collect()
}

fn referenced_access_plan_ids<'a>(
    access_plan_ids: &'a [i64],
    rules: &'a [ValidatedQuotaRule],
) -> BTreeSet<&'a i64> {
    access_plan_ids
        .iter()
        .chain(rules.iter().flat_map(|rule| rule.access_plan_ids.iter()))
        .collect()
}

async fn insert_quota_rule(
    tx: &mut Transaction<'_, Postgres>,
    plan_id: i64,
    rule: ValidatedQuotaRule,
    now: DateTime<Utc>,
) -> Result<i64, gql::BillingError> {
    let rule_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO subscription_quota_rules \
         (subscription_plan_id,rule_key,name,quota_class,amount_micros,rollover_mode, \
          rollover_cap_micros,carry_duration_seconds,created_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) RETURNING id",
    )
    .bind(plan_id)
    .bind(format!("quota-{}", uuid::Uuid::new_v4().simple()))
    .bind(rule.name)
    .bind(quota_class_to_wire(rule.quota_class))
    .bind(rule.amount_micros)
    .bind(rollover_to_wire(rule.rollover_mode))
    .bind(rule.rollover_cap_micros)
    .bind(rule.carry_duration_seconds)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    replace_quota_rule_access_plans(tx, rule_id, &rule.access_plan_ids, now).await?;
    Ok(rule_id)
}

async fn update_quota_rule(
    tx: &mut Transaction<'_, Postgres>,
    rule_id: i64,
    rule: ValidatedQuotaRule,
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    sqlx::query(
        "UPDATE subscription_quota_rules SET name=$1,quota_class=$2,amount_micros=$3, \
         rollover_mode=$4,rollover_cap_micros=$5,carry_duration_seconds=$6,updated_at=$7 \
         WHERE id=$8",
    )
    .bind(rule.name)
    .bind(quota_class_to_wire(rule.quota_class))
    .bind(rule.amount_micros)
    .bind(rollover_to_wire(rule.rollover_mode))
    .bind(rule.rollover_cap_micros)
    .bind(rule.carry_duration_seconds)
    .bind(now)
    .bind(rule_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    replace_quota_rule_access_plans(tx, rule_id, &rule.access_plan_ids, now).await
}

async fn replace_quota_rule_access_plans(
    tx: &mut Transaction<'_, Postgres>,
    rule_id: i64,
    access_plan_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    sqlx::query("DELETE FROM subscription_quota_rule_access_plans WHERE quota_rule_id=$1")
        .bind(rule_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    for access_plan_id in access_plan_ids {
        sqlx::query(
            "INSERT INTO subscription_quota_rule_access_plans \
             (quota_rule_id,access_plan_id,created_at) VALUES($1,$2,$3)",
        )
        .bind(rule_id)
        .bind(*access_plan_id)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

async fn subscription_plan_access_versions(
    tx: &mut Transaction<'_, Postgres>,
    plan_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<(i64, i64)>, gql::BillingError> {
    let expected = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::BIGINT FROM subscription_plan_access_plans \
         WHERE subscription_plan_id=$1",
    )
    .bind(plan_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    let rows = sqlx::query(
        "SELECT DISTINCT ON (s.access_plan_id) s.access_plan_id, \
                v.id AS access_plan_version_id \
         FROM subscription_plan_access_plans s \
         JOIN access_plans p ON p.id=s.access_plan_id \
         JOIN access_plan_versions v ON v.access_plan_id=p.id \
         WHERE s.subscription_plan_id=$1 AND p.status='enabled' \
           AND v.status='published' \
           AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
           AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) \
         ORDER BY s.access_plan_id,v.version DESC",
    )
    .bind(plan_id)
    .bind(now)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?;
    if i64::try_from(rows.len()).unwrap_or(i64::MAX) != expected {
        return Err(gql::BillingError::Invalid(
            "every subscription access plan must have an effective published version".into(),
        ));
    }
    Ok(rows
        .into_iter()
        .map(|row| (row.get("access_plan_id"), row.get("access_plan_version_id")))
        .collect())
}

async fn snapshot_subscription_access_versions(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
    access_versions: &[(i64, i64)],
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    for (access_plan_id, access_plan_version_id) in access_versions {
        sqlx::query(
            "INSERT INTO user_subscription_access_plan_snapshots \
             (subscription_id,access_plan_id,access_plan_version_id,created_at) \
             VALUES($1,$2,$3,$4)",
        )
        .bind(subscription_id)
        .bind(*access_plan_id)
        .bind(*access_plan_version_id)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

async fn subscription_access_versions(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
) -> Result<Vec<(i64, i64)>, gql::BillingError> {
    Ok(sqlx::query(
        "SELECT access_plan_id,access_plan_version_id \
         FROM user_subscription_access_plan_snapshots \
         WHERE subscription_id=$1 ORDER BY access_plan_id",
    )
    .bind(subscription_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?
    .into_iter()
    .map(|row| (row.get("access_plan_id"), row.get("access_plan_version_id")))
    .collect())
}

async fn published_access_version_snapshots(
    tx: &mut Transaction<'_, Postgres>,
    access_plan_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<Vec<AccessVersionSnapshot>, gql::BillingError> {
    let mut snapshots = Vec::with_capacity(access_plan_ids.len());
    for access_plan_id in access_plan_ids {
        let row = sqlx::query(
            "SELECT p.name,v.id AS access_plan_version_id FROM access_plans p \
             JOIN access_plan_versions v ON v.access_plan_id=p.id \
             WHERE p.id=$1 AND p.status='enabled' AND v.status='published' \
               AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
               AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) \
             ORDER BY v.version DESC LIMIT 1 FOR SHARE OF p,v",
        )
        .bind(*access_plan_id)
        .bind(now)
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            gql::BillingError::NotFound(format!(
                "enabled access plan {access_plan_id} with an effective published version"
            ))
        })?;
        snapshots.push(AccessVersionSnapshot {
            access_plan_id: *access_plan_id,
            access_plan_version_id: row.get("access_plan_version_id"),
            name: row.get("name"),
        });
    }
    Ok(snapshots)
}

async fn snapshot_subscription_quota_rules(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
    plan_id: i64,
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    let rules = sqlx::query(
        "SELECT id,rule_key,name,quota_class,amount_micros,rollover_mode, \
                rollover_cap_micros,carry_duration_seconds \
         FROM subscription_quota_rules WHERE subscription_plan_id=$1 \
         ORDER BY id FOR SHARE",
    )
    .bind(plan_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?;
    if rules.is_empty() {
        return Err(gql::BillingError::Invalid(
            "subscription plan has no quota rules".into(),
        ));
    }
    for rule in rules {
        let rule_id: i64 = rule.get("id");
        let quota_class: String = rule.get("quota_class");
        let access_plan_ids = sqlx::query_scalar::<_, i64>(
            "SELECT access_plan_id FROM subscription_quota_rule_access_plans \
             WHERE quota_rule_id=$1 ORDER BY access_plan_id",
        )
        .bind(rule_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
        if quota_class == "GENERAL" && !access_plan_ids.is_empty() {
            return Err(gql::BillingError::Invalid(format!(
                "general quota rule {rule_id} unexpectedly has access plans"
            )));
        }
        if quota_class == "DEDICATED" && access_plan_ids.is_empty() {
            return Err(gql::BillingError::Invalid(format!(
                "dedicated quota rule {rule_id} has no access plans"
            )));
        }
        let access_versions = published_access_version_snapshots(tx, &access_plan_ids, now).await?;
        sqlx::query(
            "INSERT INTO user_subscription_quota_rule_snapshots \
             (subscription_id,rule_key,rule_name,quota_class,amount_micros,rollover_mode, \
              rollover_cap_micros,carry_duration_seconds,access_plan_versions,created_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(subscription_id)
        .bind(rule.get::<String, _>("rule_key"))
        .bind(rule.get::<String, _>("name"))
        .bind(quota_class)
        .bind(rule.get::<i64, _>("amount_micros"))
        .bind(rule.get::<String, _>("rollover_mode"))
        .bind(
            rule.try_get::<Option<i64>, _>("rollover_cap_micros")
                .ok()
                .flatten(),
        )
        .bind(
            rule.try_get::<Option<i64>, _>("carry_duration_seconds")
                .ok()
                .flatten(),
        )
        .bind(sqlx::types::Json(access_versions))
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

async fn materialize_entitlement_items(
    tx: &mut Transaction<'_, Postgres>,
    entitlement_snapshot_id: i64,
    quota_rule_snapshot_id: i64,
    access_versions: &[AccessVersionSnapshot],
) -> Result<(Vec<i64>, Vec<String>), gql::BillingError> {
    let version_ids = access_versions
        .iter()
        .map(|snapshot| snapshot.access_plan_version_id)
        .collect::<Vec<_>>();
    let public_model_ids = if version_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT i.public_model_id FROM access_plan_items i \
             WHERE i.access_plan_version_id=ANY($1) ORDER BY i.public_model_id",
        )
        .bind(&version_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?
    };
    for public_model_id in &public_model_ids {
        sqlx::query(
            "INSERT INTO subscription_entitlement_snapshot_items \
             (snapshot_id,quota_rule_snapshot_id,public_model_id) VALUES($1,$2,$3) \
             ON CONFLICT DO NOTHING",
        )
        .bind(entitlement_snapshot_id)
        .bind(quota_rule_snapshot_id)
        .bind(*public_model_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    let model_ids = if public_model_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT model_id FROM models WHERE id=ANY($1) ORDER BY LOWER(model_id),id",
        )
        .bind(&public_model_ids)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?
    };
    Ok((public_model_ids, model_ids))
}

#[allow(clippy::too_many_arguments)]
async fn issue_period_buckets(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    bucket_status: &str,
    rollover_source_period_start: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    let entitlement_snapshot_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO subscription_entitlement_snapshots \
         (subscription_id,period_start,period_end,created_at) VALUES($1,$2,$3,$4) \
         ON CONFLICT(subscription_id,period_start) DO UPDATE SET period_end=EXCLUDED.period_end \
         RETURNING id",
    )
    .bind(subscription_id)
    .bind(period_start)
    .bind(period_end)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;

    let rule_snapshots = sqlx::query(
        "SELECT s.id,s.rule_key,s.rule_name,s.quota_class,s.amount_micros,s.rollover_mode, \
                s.rollover_cap_micros,s.carry_duration_seconds,s.access_plan_versions,q.id AS quota_rule_id \
         FROM user_subscription_quota_rule_snapshots s \
         JOIN user_subscriptions u ON u.id=s.subscription_id \
         LEFT JOIN subscription_quota_rules q ON q.subscription_plan_id=u.plan_id AND q.rule_key=s.rule_key \
         WHERE s.subscription_id=$1 ORDER BY s.id FOR SHARE OF s",
    )
    .bind(subscription_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(storage)?;
    if rule_snapshots.is_empty() {
        return Err(gql::BillingError::Invalid(
            "subscription has no quota rule snapshots".into(),
        ));
    }

    for rule in &rule_snapshots {
        let rule_snapshot_id: i64 = rule.get("id");
        let access_versions =
            parse_access_version_snapshot(rule.get::<Value, _>("access_plan_versions"))?;
        let (_, model_ids) = materialize_entitlement_items(
            tx,
            entitlement_snapshot_id,
            rule_snapshot_id,
            &access_versions,
        )
        .await?;
        let scope_snapshot = json!({
            "quotaClass": rule.get::<String, _>("quota_class"),
            "accessPlans": access_versions,
            "modelIDs": model_ids,
        });
        sqlx::query(
            "INSERT INTO subscription_allowance_buckets \
             (subscription_id,quota_rule_id,quota_rule_snapshot_id,entitlement_snapshot_id, \
              quota_class,scope_snapshot,issued_at,period_start,period_end,expires_at, \
              carryover_expires_at,source_bucket_id,granted_micros,consumed_micros, \
              reserved_micros,rollover_micros,status,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9,NULL,NULL,$10,0,0,0,$11,$7,$7) \
             ON CONFLICT(subscription_id,quota_rule_snapshot_id,period_start) \
             WHERE source_bucket_id IS NULL DO NOTHING",
        )
        .bind(subscription_id)
        .bind(
            rule.try_get::<Option<i64>, _>("quota_rule_id")
                .ok()
                .flatten(),
        )
        .bind(rule_snapshot_id)
        .bind(entitlement_snapshot_id)
        .bind(rule.get::<String, _>("quota_class"))
        .bind(sqlx::types::Json(scope_snapshot))
        .bind(now)
        .bind(period_start)
        .bind(period_end)
        .bind(rule.get::<i64, _>("amount_micros"))
        .bind(bucket_status)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }

    if let Some(source_period_start) = rollover_source_period_start {
        let source_buckets = sqlx::query(
            "SELECT b.id,b.quota_rule_id,b.quota_rule_snapshot_id,b.entitlement_snapshot_id, \
                    b.quota_class,b.scope_snapshot,b.period_end,b.granted_micros,b.consumed_micros, \
                    b.reserved_micros,r.rollover_mode,r.rollover_cap_micros,r.carry_duration_seconds \
             FROM subscription_allowance_buckets b \
             JOIN user_subscription_quota_rule_snapshots r ON r.id=b.quota_rule_snapshot_id \
             WHERE b.subscription_id=$1 AND b.period_start=$2 AND b.source_bucket_id IS NULL \
               AND b.status IN ('active','paused') FOR UPDATE OF b",
        )
        .bind(subscription_id)
        .bind(source_period_start)
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
        for source in source_buckets {
            let source_id: i64 = source.get("id");
            let reserved_micros: i64 = source.get("reserved_micros");
            let available = (source.get::<i64, _>("granted_micros")
                - source.get::<i64, _>("consumed_micros")
                - reserved_micros)
                .max(0);
            if source.get::<String, _>("rollover_mode") == "capped" && available > 0 {
                let cap = source
                    .try_get::<Option<i64>, _>("rollover_cap_micros")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                let duration = source
                    .try_get::<Option<i64>, _>("carry_duration_seconds")
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                let carry = available.min(cap);
                let carry_expires_at = source
                    .get::<DateTime<Utc>, _>("period_end")
                    .checked_add_signed(Duration::seconds(duration))
                    .ok_or_else(|| {
                        gql::BillingError::Invalid("carryover expiry overflow".into())
                    })?;
                if carry > 0 && carry_expires_at > now {
                    sqlx::query(
                        "INSERT INTO subscription_allowance_buckets \
                         (subscription_id,quota_rule_id,quota_rule_snapshot_id,entitlement_snapshot_id, \
                          quota_class,scope_snapshot,issued_at,period_start,period_end,expires_at, \
                          carryover_expires_at,source_bucket_id,granted_micros,consumed_micros, \
                          reserved_micros,rollover_micros,status,created_at,updated_at) \
                         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$10,$11,$12,0,0,$12,$13,$7,$7) \
                         ON CONFLICT(subscription_id,quota_rule_snapshot_id,period_start,source_bucket_id) \
                         WHERE source_bucket_id IS NOT NULL DO NOTHING",
                    )
                    .bind(subscription_id)
                    .bind(
                        source
                            .try_get::<Option<i64>, _>("quota_rule_id")
                            .ok()
                            .flatten(),
                    )
                    .bind(source.get::<i64, _>("quota_rule_snapshot_id"))
                    .bind(source.get::<i64, _>("entitlement_snapshot_id"))
                    .bind(source.get::<String, _>("quota_class"))
                    .bind(source.get::<Value, _>("scope_snapshot"))
                    .bind(now)
                    .bind(period_start)
                    .bind(period_end)
                    .bind(carry_expires_at)
                    .bind(source_id)
                    .bind(carry)
                    .bind(bucket_status)
                    .execute(&mut **tx)
                    .await
                    .map_err(storage)?;
                }
            }
            sqlx::query(
                "UPDATE subscription_allowance_buckets SET status=$1,updated_at=$2 WHERE id=$3",
            )
            .bind(if reserved_micros > 0 {
                "draining"
            } else {
                "expired"
            })
            .bind(now)
            .bind(source_id)
            .execute(&mut **tx)
            .await
            .map_err(storage)?;
        }
    }

    sqlx::query(
        "UPDATE subscription_allowance_buckets SET \
         status=CASE WHEN reserved_micros>0 THEN 'draining' ELSE 'expired' END,updated_at=$1 \
         WHERE subscription_id=$2 AND status IN ('active','paused') AND expires_at<=$1",
    )
    .bind(now)
    .bind(subscription_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn sync_subscription_access_grants(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
    access_versions: &[(i64, i64)],
    valid_from: DateTime<Utc>,
    valid_until: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), gql::BillingError> {
    let project_id = sqlx::query_scalar::<_, i64>(
        "SELECT project_id FROM user_subscription_projects WHERE subscription_id=$1",
    )
    .bind(subscription_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    sqlx::query(
        "UPDATE project_access_grants SET status='inactive',valid_until=$1,updated_at=$2 \
         WHERE project_id=$3 AND source_type='subscription' AND source_id LIKE $4",
    )
    .bind(valid_from)
    .bind(now)
    .bind(project_id)
    .bind(format!(
        "{}%",
        subscription_grant_source_prefix(subscription_id)
    ))
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    for (access_plan_id, version_id) in access_versions {
        sqlx::query(
            "INSERT INTO project_access_grants \
             (project_id,access_plan_version_id,source_type,source_id,status, \
              valid_from,valid_until,created_at,updated_at) \
             VALUES($1,$2,'subscription',$3,'active',$4,$5,$6,$6) \
             ON CONFLICT(project_id,source_type,source_id) DO UPDATE SET \
               access_plan_version_id=EXCLUDED.access_plan_version_id,status='active', \
               valid_from=EXCLUDED.valid_from,valid_until=EXCLUDED.valid_until, \
               updated_at=EXCLUDED.updated_at",
        )
        .bind(project_id)
        .bind(*version_id)
        .bind(subscription_grant_source_id(
            subscription_id,
            *access_plan_id,
        ))
        .bind(valid_from)
        .bind(valid_until)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

async fn refresh_locked(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
) -> Result<(), gql::BillingError> {
    let subscription = sqlx::query(
        "SELECT s.plan_id,s.status,s.current_period_start,s.current_period_end, \
                s.assigned_interval_unit AS interval_unit, \
                s.assigned_interval_count AS interval_count,p.status AS plan_status \
         FROM user_subscriptions s JOIN subscription_plans p ON p.id=s.plan_id \
         WHERE s.id=$1 FOR UPDATE OF s",
    )
    .bind(subscription_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
    let subscription_status: String = subscription.get("status");
    if !matches!(subscription_status.as_str(), "active" | "paused") {
        return Err(gql::BillingError::Invalid(
            "only an active or paused subscription can refresh".into(),
        ));
    }
    if subscription.get::<String, _>("plan_status") != "enabled" {
        return Err(gql::BillingError::Invalid(
            "subscription plan is not enabled".into(),
        ));
    }
    let now = Utc::now();
    let mut start: DateTime<Utc> = subscription.get("current_period_start");
    let mut end: DateTime<Utc> = subscription.get("current_period_end");
    if end > now {
        return Ok(());
    }
    lock_live_model_group_versions(tx).await?;
    let rollover_source_start = start;
    while end <= now {
        start = end;
        let interval_count = positive_interval_count(subscription.get("interval_count"))?;
        end = next_period(
            start,
            subscription.get::<String, _>("interval_unit").as_str(),
            interval_count,
        )?;
    }
    issue_period_buckets(
        tx,
        subscription_id,
        start,
        end,
        if subscription_status == "paused" {
            "paused"
        } else {
            "active"
        },
        Some(rollover_source_start),
        now,
    )
    .await?;
    sqlx::query(
        "UPDATE user_subscriptions SET current_period_start=$1,current_period_end=$2,updated_at=$3 \
         WHERE id=$4",
    )
    .bind(start)
    .bind(end)
    .bind(now)
    .bind(subscription_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    let versions = subscription_access_versions(tx, subscription_id).await?;
    sync_subscription_access_grants(tx, subscription_id, &versions, start, end, now).await?;
    if subscription_status == "paused" {
        sqlx::query(
            "UPDATE project_access_grants SET status='paused',updated_at=$1 \
             WHERE source_type='subscription' AND source_id LIKE $2 AND status='active'",
        )
        .bind(now)
        .bind(format!(
            "{}%",
            subscription_grant_source_prefix(subscription_id)
        ))
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    Ok(())
}

/// Serialize subscription grant materialization with model-group publishing.
/// This closes the race where billing could read an archived version while a
/// concurrent model-group transaction advances the live subscription pointer.
async fn lock_live_model_group_versions(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), gql::BillingError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('conduit.simple_groups', 0))")
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    Ok(())
}

async fn set_inactive_locked(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
    allowed_statuses: &[&str],
    target_status: &str,
    disable_auto_renew: bool,
) -> Result<(), gql::BillingError> {
    let current = sqlx::query_scalar::<_, String>(
        "SELECT status FROM user_subscriptions WHERE id=$1 FOR UPDATE",
    )
    .bind(subscription_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
    if current == target_status {
        return Ok(());
    }
    if !allowed_statuses.contains(&current.as_str()) {
        return Err(gql::BillingError::Invalid(format!(
            "subscription cannot transition from {current} to {target_status}"
        )));
    }
    let now = Utc::now();
    let pattern = format!("{}%", subscription_grant_source_prefix(subscription_id));
    sqlx::query(
        "UPDATE project_access_grants SET status=$1,updated_at=$2 \
         WHERE source_type='subscription' AND source_id LIKE $3 \
           AND status IN ('active','paused')",
    )
    .bind(target_status)
    .bind(now)
    .bind(pattern)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    if target_status == "expired" {
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET \
             status=CASE WHEN reserved_micros>0 THEN 'draining' ELSE 'expired' END,updated_at=$1 \
             WHERE subscription_id=$2 AND status IN ('active','paused')",
        )
        .bind(now)
        .bind(subscription_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    } else {
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET status=$1,updated_at=$2 \
             WHERE subscription_id=$3 AND status IN ('active','paused')",
        )
        .bind(target_status)
        .bind(now)
        .bind(subscription_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    sqlx::query(
        "UPDATE user_subscriptions SET status=$1, \
         auto_renew=CASE WHEN $2 THEN FALSE ELSE auto_renew END,updated_at=$3 WHERE id=$4",
    )
    .bind(target_status)
    .bind(disable_auto_renew)
    .bind(now)
    .bind(subscription_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

async fn cancel_pending_locked(
    tx: &mut Transaction<'_, Postgres>,
    subscription_id: i64,
) -> Result<(), gql::BillingError> {
    let current = sqlx::query_scalar::<_, String>(
        "SELECT status FROM user_subscriptions WHERE id=$1 FOR UPDATE",
    )
    .bind(subscription_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or_else(|| gql::BillingError::NotFound(format!("subscription {subscription_id}")))?;
    if current == "cancel_pending" {
        return Ok(());
    }
    if !matches!(current.as_str(), "active" | "paused") {
        return Err(gql::BillingError::Invalid(format!(
            "subscription cannot transition from {current} to cancel_pending"
        )));
    }
    let now = Utc::now();
    if current == "paused" {
        sqlx::query(
            "UPDATE project_access_grants SET status='active',updated_at=$1 \
             WHERE source_type='subscription' AND source_id LIKE $2 AND status='paused' \
               AND (valid_until IS NULL OR valid_until>$1)",
        )
        .bind(now)
        .bind(format!(
            "{}%",
            subscription_grant_source_prefix(subscription_id)
        ))
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET status='active',updated_at=$1 \
             WHERE subscription_id=$2 AND status='paused' AND expires_at>$1",
        )
        .bind(now)
        .bind(subscription_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET \
             status=CASE WHEN reserved_micros>0 THEN 'draining' ELSE 'expired' END,updated_at=$1 \
             WHERE subscription_id=$2 AND status='paused' AND expires_at<=$1",
        )
        .bind(now)
        .bind(subscription_id)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    }
    sqlx::query(
        "UPDATE user_subscriptions SET status='cancel_pending',auto_renew=FALSE,updated_at=$1 \
         WHERE id=$2",
    )
    .bind(now)
    .bind(subscription_id)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

fn subscription_grant_source_prefix(subscription_id: i64) -> String {
    format!("{subscription_id}:")
}

fn subscription_grant_source_id(subscription_id: i64, access_plan_id: i64) -> String {
    format!(
        "{}{access_plan_id}",
        subscription_grant_source_prefix(subscription_id)
    )
}

#[async_trait::async_trait]
impl gql::BillingServices for PgBillingAdapter {
    async fn user_balance(&self, user_id: &str) -> Result<gql::UserBalance, gql::BillingError> {
        self.user_balance_value(parse_id(user_id)?).await
    }

    async fn project_balance(
        &self,
        project_id: &str,
    ) -> Result<gql::ProjectBalance, gql::BillingError> {
        self.project_balance_value(parse_id(project_id)?).await
    }

    async fn project_wallet_comparison(
        &self,
        project_id: &str,
    ) -> Result<gql::ProjectWalletComparison, gql::BillingError> {
        let project_id = parse_id(project_id)?;
        let project = self.project_balance_value(project_id).await?;
        let owners = sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM user_projects WHERE project_id=$1 AND is_owner=TRUE \
             ORDER BY user_id LIMIT 2",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let (owner_user_id, legacy, owner_status) = match owners.as_slice() {
            [owner] => (
                Some(*owner),
                Some(self.user_balance(&owner.to_string()).await?),
                None,
            ),
            [] => (None, None, Some("missing_owner")),
            _ => (None, None, Some("ambiguous_owner")),
        };
        let legacy_credit = legacy
            .as_ref()
            .map(|v| v.credit_balance.clone())
            .unwrap_or_else(|| "0".into());
        let legacy_subscription = legacy
            .as_ref()
            .map(|v| v.subscription_balance.clone())
            .unwrap_or_else(|| "0".into());
        let legacy_available = legacy
            .as_ref()
            .map(|v| v.available_balance.clone())
            .unwrap_or_else(|| "0".into());
        let delta = parse_signed_amount(&project.available_balance)?
            - parse_signed_amount(&legacy_available)?;
        let status = owner_status.unwrap_or_else(|| {
            if project.wallet_status == "uninitialized" {
                "project_wallet_uninitialized"
            } else if delta == 0
                && project.credit_balance == legacy_credit
                && project.subscription_balance == legacy_subscription
            {
                "match"
            } else {
                "different"
            }
        });
        Ok(gql::ProjectWalletComparison {
            project_id: project_node_id(project_id),
            owner_user_id: owner_user_id.map(user_node_id),
            status: status.into(),
            legacy_credit_balance: legacy_credit,
            project_credit_balance: project.credit_balance.clone(),
            legacy_subscription_balance: legacy_subscription,
            project_subscription_balance: project.subscription_balance.clone(),
            legacy_available_balance: legacy_available,
            project_available_balance: project.available_balance,
            available_delta: amount(delta),
            generated_at: wire_time(Utc::now()),
        })
    }

    async fn user_project_balance(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<gql::ProjectBalance, gql::BillingError> {
        let user_id = parse_id(user_id)?;
        let project_id = parse_id(project_id)?;
        ensure_user(&self.pool, user_id).await?;
        ensure_project_membership(&self.pool, user_id, project_id).await?;
        self.project_balance_value(project_id).await
    }

    async fn user_project_wallet_comparison(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<gql::ProjectWalletComparison, gql::BillingError> {
        let user_id = parse_id(user_id)?;
        let project_id = parse_id(project_id)?;
        ensure_user(&self.pool, user_id).await?;
        ensure_project_membership(&self.pool, user_id, project_id).await?;
        self.project_wallet_comparison(&project_id.to_string())
            .await
    }

    async fn subscription_plans(&self) -> Result<Vec<gql::SubscriptionPlan>, gql::BillingError> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM subscription_plans ORDER BY LOWER(name),id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut plans = Vec::with_capacity(ids.len());
        for plan_id in ids {
            plans.push(self.plan(plan_id).await?);
        }
        Ok(plans)
    }

    async fn user_subscriptions(
        &self,
        user_id: &str,
    ) -> Result<Vec<gql::UserSubscription>, gql::BillingError> {
        let user_id = parse_id(user_id)?;
        ensure_user(&self.pool, user_id).await?;
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM user_subscriptions WHERE user_id=$1 ORDER BY created_at DESC,id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut subscriptions = Vec::with_capacity(ids.len());
        for subscription_id in ids {
            subscriptions.push(self.subscription(subscription_id).await?);
        }
        Ok(subscriptions)
    }

    async fn user_project_subscriptions(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<gql::UserSubscription>, gql::BillingError> {
        let user_id = parse_id(user_id)?;
        let project_id = parse_id(project_id)?;
        ensure_user(&self.pool, user_id).await?;
        ensure_project_membership(&self.pool, user_id, project_id).await?;
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT s.id FROM user_subscriptions s \
             JOIN user_subscription_projects sp ON sp.subscription_id=s.id \
             WHERE s.user_id=$1 AND sp.project_id=$2 ORDER BY s.created_at DESC,s.id DESC",
        )
        .bind(user_id)
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut subscriptions = Vec::with_capacity(ids.len());
        for subscription_id in ids {
            subscriptions.push(self.subscription(subscription_id).await?);
        }
        Ok(subscriptions)
    }

    async fn subscription_projects(
        &self,
        user_id: &str,
    ) -> Result<Vec<gql::SubscriptionProjectOption>, gql::BillingError> {
        let user_id = parse_id(user_id)?;
        ensure_user(&self.pool, user_id).await?;
        Ok(sqlx::query(
            "SELECT p.id,p.name,p.status,(cp.project_id IS NOT NULL) AS commercial_policy_active \
             FROM user_projects up JOIN projects p ON p.id=up.project_id \
             LEFT JOIN project_commercial_profiles cp ON cp.project_id=p.id AND cp.status='active' \
             WHERE up.user_id=$1 AND p.deleted_at=0 ORDER BY LOWER(p.name),p.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(|row| gql::SubscriptionProjectOption {
            id: project_node_id(row.get("id")),
            name: row.get("name"),
            status: row.get("status"),
            commercial_policy_active: row.get("commercial_policy_active"),
        })
        .collect())
    }

    async fn grant_user_credit(
        &self,
        _input: gql::GrantUserCreditInput,
    ) -> Result<gql::UserBalance, gql::BillingError> {
        Err(gql::BillingError::Invalid(
            "user-scoped credit grants are legacy; grant credit to a selected Project".into(),
        ))
    }

    async fn grant_project_credit(
        &self,
        input: gql::GrantProjectCreditInput,
    ) -> Result<gql::ProjectBalance, gql::BillingError> {
        let project_id = parse_id(input.project_id.as_str())?;
        ensure_project(&self.pool, project_id).await?;
        let micros = parse_positive_amount(&input.amount)?;
        let key = nonempty(input.idempotency_key, "idempotencyKey")?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // The advisory lock makes same-key retries deterministic even if they
        // target different wallets and race before the unique insert.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(&key)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        if let Some(existing) = sqlx::query(
            "SELECT w.project_id,w.currency,e.amount_micros \
             FROM project_credit_ledger_entries e \
             JOIN project_wallets w ON w.id=e.wallet_id WHERE e.idempotency_key=$1",
        )
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        {
            if existing.get::<i64, _>("project_id") != project_id
                || existing.get::<String, _>("currency") != STATION_CREDIT_CODE
                || existing.get::<i64, _>("amount_micros") != micros
            {
                return Err(gql::BillingError::Invalid(
                    "idempotencyKey was already used for a different Project credit grant".into(),
                ));
            }
            tx.commit().await.map_err(storage)?;
            return self.project_balance_value(project_id).await;
        }
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO project_wallets(project_id,currency,status,created_at,updated_at) \
             VALUES($1,$2,'active',$3,$3) ON CONFLICT(project_id,currency) DO NOTHING",
        )
        .bind(project_id)
        .bind(STATION_CREDIT_CODE)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        let wallet_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM project_wallets WHERE project_id=$1 AND currency=$2 FOR UPDATE",
        )
        .bind(project_id)
        .bind(STATION_CREDIT_CODE)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO project_credit_ledger_entries \
             (wallet_id,amount_micros,entry_type,reference_type,reference_id, \
              idempotency_key,description,metadata,created_at) \
             VALUES($1,$2,'admin_grant','project',$3,$4,$5,'{}',$6)",
        )
        .bind(wallet_id)
        .bind(micros)
        .bind(project_id.to_string())
        .bind(key)
        .bind(input.description)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.project_balance_value(project_id).await
    }

    async fn create_subscription_plan(
        &self,
        input: gql::CreateSubscriptionPlanInput,
    ) -> Result<gql::SubscriptionPlan, gql::BillingError> {
        let name = nonempty(input.name, "name")?;
        let interval_count = input.interval_count.unwrap_or(1);
        if interval_count <= 0 {
            return Err(gql::BillingError::Invalid(
                "intervalCount must be positive".into(),
            ));
        }
        let quota_rules = validate_quota_rules(input.quota_rules)?;
        if quota_rules.iter().any(|rule| rule.id.is_some()) {
            return Err(gql::BillingError::Invalid(
                "quota rule IDs cannot be supplied when creating a plan".into(),
            ));
        }
        let access_plan_ids = parse_access_plan_ids(input.access_plan_ids)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let referenced_access_plan_ids = referenced_access_plan_ids(&access_plan_ids, &quota_rules);
        for access_plan_id in referenced_access_plan_ids {
            ensure_access_plan_tx(&mut tx, *access_plan_id).await?;
        }
        let plan_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO subscription_plans \
             (name,currency,interval_unit,interval_count,status,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'enabled',$5,$5) RETURNING id",
        )
        .bind(name)
        .bind(STATION_CREDIT_CODE)
        .bind(interval_to_wire(input.interval_unit))
        .bind(interval_count)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        for access_plan_id in access_plan_ids {
            sqlx::query(
                "INSERT INTO subscription_plan_access_plans \
                 (subscription_plan_id,access_plan_id,created_at,updated_at) \
                 VALUES($1,$2,$3,$3)",
            )
            .bind(plan_id)
            .bind(access_plan_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        for rule in quota_rules {
            insert_quota_rule(&mut tx, plan_id, rule, now).await?;
        }
        tx.commit().await.map_err(storage)?;
        self.plan(plan_id).await
    }

    async fn update_subscription_plan(
        &self,
        input: gql::UpdateSubscriptionPlanInput,
    ) -> Result<gql::SubscriptionPlan, gql::BillingError> {
        let plan_id = parse_id(input.id.as_str())?;
        let name = nonempty(input.name, "name")?;
        if input.interval_count <= 0 {
            return Err(gql::BillingError::Invalid(
                "intervalCount must be positive".into(),
            ));
        }
        let quota_rules = validate_quota_rules(input.quota_rules)?;
        let access_plan_ids = parse_access_plan_ids(input.access_plan_ids)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;
        if sqlx::query_scalar::<_, i64>("SELECT id FROM subscription_plans WHERE id=$1 FOR UPDATE")
            .bind(plan_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .is_none()
        {
            return Err(gql::BillingError::NotFound(format!(
                "subscription plan {plan_id}"
            )));
        }
        let referenced_access_plan_ids = referenced_access_plan_ids(&access_plan_ids, &quota_rules);
        for access_plan_id in referenced_access_plan_ids {
            ensure_access_plan_tx(&mut tx, *access_plan_id).await?;
        }
        sqlx::query(
            "UPDATE subscription_plans SET name=$1,currency=$2,interval_unit=$3, \
             interval_count=$4,status=$5,updated_at=$6 WHERE id=$7",
        )
        .bind(name)
        .bind(STATION_CREDIT_CODE)
        .bind(interval_to_wire(input.interval_unit))
        .bind(input.interval_count)
        .bind(status_to_wire(input.status))
        .bind(now)
        .bind(plan_id)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query("DELETE FROM subscription_plan_access_plans WHERE subscription_plan_id=$1")
            .bind(plan_id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        for access_plan_id in access_plan_ids {
            sqlx::query(
                "INSERT INTO subscription_plan_access_plans \
                 (subscription_plan_id,access_plan_id,created_at,updated_at) \
                 VALUES($1,$2,$3,$3)",
            )
            .bind(plan_id)
            .bind(access_plan_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        let existing_rule_ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM subscription_quota_rules WHERE subscription_plan_id=$1 FOR UPDATE",
        )
        .bind(plan_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?
        .into_iter()
        .collect::<BTreeSet<_>>();
        let mut retained_rule_ids = BTreeSet::new();
        for rule in quota_rules {
            if let Some(rule_id) = rule.id {
                if !existing_rule_ids.contains(&rule_id) {
                    return Err(gql::BillingError::Invalid(format!(
                        "quota rule {rule_id} does not belong to subscription plan {plan_id}"
                    )));
                }
                if !retained_rule_ids.insert(rule_id) {
                    return Err(gql::BillingError::Invalid(format!(
                        "quota rule {rule_id} was supplied more than once"
                    )));
                }
                update_quota_rule(&mut tx, rule_id, rule, now).await?;
            } else {
                let rule_id = insert_quota_rule(&mut tx, plan_id, rule, now).await?;
                retained_rule_ids.insert(rule_id);
            }
        }
        let retained_rule_ids = retained_rule_ids.into_iter().collect::<Vec<_>>();
        sqlx::query(
            "DELETE FROM subscription_quota_rules \
             WHERE subscription_plan_id=$1 AND NOT (id=ANY($2))",
        )
        .bind(plan_id)
        .bind(&retained_rule_ids)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        // Assignment terms and the selected model-group IDs are snapshots.
        // Existing subscriptions are not rewritten by plan edits.
        tx.commit().await.map_err(storage)?;
        self.plan(plan_id).await
    }

    async fn assign_user_subscription(
        &self,
        input: gql::AssignUserSubscriptionInput,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let user_id = parse_id(input.user_id.as_str())?;
        let plan_id = parse_id(input.plan_id.as_str())?;
        let project_id = parse_id(input.project_id.as_str())?;
        let assignment_key = nonempty(input.idempotency_key, "idempotencyKey")?;
        let initial_auto_renew = input.auto_renew.unwrap_or(true);
        let assignment_request_snapshot = json!({
            "schemaVersion": 1,
            "userID": user_id,
            "planID": plan_id,
            "projectID": project_id,
            "autoRenew": initial_auto_renew,
            "intervalUnit": input.interval_unit.map(interval_to_wire),
            "intervalCount": input.interval_count,
        });
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // Assignment keys are global, but the lock namespace is operation-specific
        // so an unrelated idempotent operation may reuse the same caller key.
        sqlx::query(
            "SELECT pg_advisory_xact_lock(\
             hashtextextended('conduit.billing.assign_user_subscription:' || $1::text,0))",
        )
        .bind(&assignment_key)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        if let Some(existing) = sqlx::query(
            "SELECT id,assignment_request_snapshot FROM user_subscriptions \
             WHERE assignment_key=$1",
        )
        .bind(&assignment_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        {
            if existing.get::<Value, _>("assignment_request_snapshot")
                != assignment_request_snapshot
            {
                return Err(gql::BillingError::Invalid(
                    "idempotencyKey was already used for a different subscription assignment"
                        .into(),
                ));
            }
            let subscription_id = existing.get::<i64, _>("id");
            tx.commit().await.map_err(storage)?;
            return self.subscription(subscription_id).await;
        }
        ensure_user_tx(&mut tx, user_id).await?;
        if sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_projects up JOIN projects p ON p.id=up.project_id \
             WHERE up.user_id=$1 AND up.project_id=$2 AND p.status='active' AND p.deleted_at=0)",
        )
        .bind(user_id)
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?
            == false
        {
            return Err(gql::BillingError::Invalid(format!(
                "user {user_id} is not a member of active project {project_id}"
            )));
        }
        let plan = sqlx::query(
            "SELECT name,status,interval_unit,interval_count \
             FROM subscription_plans WHERE id=$1 FOR SHARE",
        )
        .bind(plan_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::BillingError::NotFound(format!("subscription plan {plan_id}")))?;
        if plan.get::<String, _>("status") != "enabled" {
            return Err(gql::BillingError::Invalid(
                "subscription plan is not enabled".into(),
            ));
        }
        let interval_unit = input
            .interval_unit
            .unwrap_or_else(|| interval_from_wire(plan.get::<String, _>("interval_unit").as_str()));
        let interval_count = input
            .interval_count
            .unwrap_or_else(|| plan.get("interval_count"));
        if interval_count <= 0 {
            return Err(gql::BillingError::Invalid(
                "intervalCount must be positive".into(),
            ));
        }
        let now = Utc::now();
        let end = next_period(
            now,
            interval_to_wire(interval_unit),
            positive_interval_count(interval_count)?,
        )?;
        lock_live_model_group_versions(&mut tx).await?;
        let versions = subscription_plan_access_versions(&mut tx, plan_id, now).await?;
        let subscription_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot,status, \
              current_period_start,current_period_end,assigned_interval_unit, \
              assigned_interval_count,auto_renew,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'active',$5,$6,$7,$8,$9,$5,$5) RETURNING id",
        )
        .bind(user_id)
        .bind(plan_id)
        .bind(assignment_key)
        .bind(assignment_request_snapshot)
        .bind(now)
        .bind(end)
        .bind(interval_to_wire(interval_unit))
        .bind(interval_count)
        .bind(initial_auto_renew)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        sqlx::query(
            "INSERT INTO user_subscription_projects(subscription_id,project_id,created_at) \
             VALUES($1,$2,$3)",
        )
        .bind(subscription_id)
        .bind(project_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        snapshot_subscription_quota_rules(&mut tx, subscription_id, plan_id, now).await?;
        issue_period_buckets(&mut tx, subscription_id, now, end, "active", None, now).await?;
        snapshot_subscription_access_versions(&mut tx, subscription_id, &versions, now).await?;
        sync_subscription_access_grants(&mut tx, subscription_id, &versions, now, end, now).await?;
        tx.commit().await.map_err(storage)?;
        self.subscription(subscription_id).await
    }

    async fn refresh_subscription_allowance(
        &self,
        subscription_id: &str,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(subscription_id)?;
        self.refresh(subscription_id, false).await?;
        self.subscription(subscription_id).await
    }

    async fn pause_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(subscription_id)?;
        self.set_inactive_status(subscription_id, &["active"], "paused", false)
            .await?;
        self.subscription(subscription_id).await
    }

    async fn resume_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(subscription_id)?;
        self.reactivate_subscription(subscription_id, "paused", false)
            .await?;
        self.subscription(subscription_id).await
    }

    async fn cancel_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(subscription_id)?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        cancel_pending_locked(&mut tx, subscription_id).await?;
        tx.commit().await.map_err(storage)?;
        self.subscription(subscription_id).await
    }

    async fn renew_user_subscription(
        &self,
        subscription_id: &str,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(subscription_id)?;
        self.reactivate_subscription(subscription_id, "expired", true)
            .await?;
        self.subscription(subscription_id).await
    }

    async fn set_subscription_auto_renew(
        &self,
        input: gql::SetSubscriptionAutoRenewInput,
    ) -> Result<gql::UserSubscription, gql::BillingError> {
        let subscription_id = parse_id(input.subscription_id.as_str())?;
        let result = sqlx::query(
            "UPDATE user_subscriptions SET auto_renew=$1,updated_at=$2 \
             WHERE id=$3 AND status IN ('active','paused')",
        )
        .bind(input.auto_renew)
        .bind(Utc::now())
        .bind(subscription_id)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        if result.rows_affected() == 0 {
            return Err(gql::BillingError::Invalid(
                "auto renewal can only be changed for an active or paused subscription".into(),
            ));
        }
        self.subscription(subscription_id).await
    }

    async fn record_commercial_operation_audit(
        &self,
        audit: gql::CommercialOperationAudit,
    ) -> Result<(), gql::BillingError> {
        sqlx::query(
            "INSERT INTO commercial_operation_audits \
             (actor_type,actor_id,operation,target_project_id,target_user_id,amount,currency, \
              plan_id,plan_name,subscription_id,idempotency_key,result,error_message,created_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(audit.actor_type)
        .bind(audit.actor_id)
        .bind(audit.operation)
        .bind(audit.target_project_id)
        .bind(audit.target_user_id)
        .bind(audit.amount)
        .bind(audit.currency)
        .bind(audit.plan_id)
        .bind(audit.plan_name)
        .bind(audit.subscription_id)
        .bind(audit.idempotency_key)
        .bind(audit.result)
        .bind(audit.error_message)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }
}

fn plan_from_row(
    row: sqlx::postgres::PgRow,
    access_plans: Vec<gql::SubscriptionAccessPlan>,
    quota_rules: Vec<gql::SubscriptionQuotaRule>,
) -> gql::SubscriptionPlan {
    let allowance_micros = quota_rules
        .iter()
        .filter_map(|rule| Decimal::from_str(&rule.allowance).ok())
        .filter_map(|value| decimal_to_micros(value).ok())
        .fold(0_i64, i64::saturating_add);
    let rollover_mode = if quota_rules
        .iter()
        .any(|rule| rule.rollover_mode == gql::RolloverMode::Capped)
    {
        gql::RolloverMode::Capped
    } else {
        gql::RolloverMode::None
    };
    let rollover_cap_micros = quota_rules
        .iter()
        .filter(|rule| rule.rollover_mode == gql::RolloverMode::Capped)
        .filter_map(|rule| rule.rollover_cap.as_deref())
        .filter_map(|value| Decimal::from_str(value).ok())
        .filter_map(|value| decimal_to_micros(value).ok())
        .fold(0_i64, i64::saturating_add);
    gql::SubscriptionPlan {
        id: id(row.get("id")),
        name: row.get("name"),
        currency: STATION_CREDIT_CODE.into(),
        allowance: amount(allowance_micros),
        interval_unit: interval_from_wire(row.get::<String, _>("interval_unit").as_str()),
        interval_count: row.get("interval_count"),
        rollover_mode,
        rollover_cap: (rollover_mode == gql::RolloverMode::Capped)
            .then(|| amount(rollover_cap_micros)),
        access_plans,
        quota_rules,
        status: status_from_wire(row.get::<String, _>("status").as_str()),
    }
}

fn quota_rule_from_row(
    row: sqlx::postgres::PgRow,
    access_plans: Vec<gql::SubscriptionAccessPlan>,
) -> gql::SubscriptionQuotaRule {
    gql::SubscriptionQuotaRule {
        id: id(row.get("id")),
        name: row.get("name"),
        quota_class: quota_class_from_wire(row.get::<String, _>("quota_class").as_str()),
        allowance: amount(row.get("amount_micros")),
        rollover_mode: if row.get::<String, _>("rollover_mode") == "capped" {
            gql::RolloverMode::Capped
        } else {
            gql::RolloverMode::None
        },
        rollover_cap: row
            .try_get::<Option<i64>, _>("rollover_cap_micros")
            .ok()
            .flatten()
            .map(amount),
        carryover_days: row
            .try_get::<Option<i64>, _>("carry_duration_seconds")
            .ok()
            .flatten()
            .and_then(|seconds| i32::try_from(seconds / SECONDS_PER_DAY).ok()),
        access_plans,
    }
}

fn parse_access_version_snapshot(
    value: Value,
) -> Result<Vec<AccessVersionSnapshot>, gql::BillingError> {
    serde_json::from_value(value).map_err(|error| {
        gql::BillingError::Storage(format!("invalid access plan version snapshot: {error}"))
    })
}

fn next_period(
    start: DateTime<Utc>,
    unit: &str,
    count: u32,
) -> Result<DateTime<Utc>, gql::BillingError> {
    match unit {
        "day" => start
            .checked_add_signed(Duration::days(i64::from(count)))
            .ok_or_else(|| gql::BillingError::Invalid("subscription period overflow".into())),
        "year" => start
            .checked_add_months(Months::new(count.saturating_mul(12)))
            .ok_or_else(|| gql::BillingError::Invalid("subscription period overflow".into())),
        _ => start
            .checked_add_months(Months::new(count))
            .ok_or_else(|| gql::BillingError::Invalid("subscription period overflow".into())),
    }
}

fn positive_interval_count(value: i32) -> Result<u32, gql::BillingError> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| gql::BillingError::Invalid("intervalCount must be positive".into()))
}

async fn read_snapshot_time(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<DateTime<Utc>, gql::BillingError> {
    sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)
}

async fn ensure_user_tx(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
) -> Result<(), gql::BillingError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND deleted_at=0)",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(gql::BillingError::NotFound(format!("user {user_id}")))
    }
}

async fn ensure_project_tx(
    tx: &mut Transaction<'_, Postgres>,
    project_id: i64,
) -> Result<(), gql::BillingError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1 AND deleted_at=0)",
    )
    .bind(project_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(gql::BillingError::NotFound(format!("project {project_id}")))
    }
}

async fn ensure_user(pool: &PgPool, user_id: i64) -> Result<(), gql::BillingError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND deleted_at=0)",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(gql::BillingError::NotFound(format!("user {user_id}")))
    }
}

async fn ensure_project(pool: &PgPool, project_id: i64) -> Result<(), gql::BillingError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE id=$1 AND deleted_at=0)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(gql::BillingError::NotFound(format!("project {project_id}")))
    }
}

async fn ensure_access_plan_tx(
    tx: &mut Transaction<'_, Postgres>,
    access_plan_id: i64,
) -> Result<(), gql::BillingError> {
    let version = sqlx::query_scalar::<_, i64>(
        "SELECT v.id FROM access_plans p \
         JOIN access_plan_versions v ON v.access_plan_id=p.id \
         WHERE p.id=$1 AND p.status='enabled' AND v.status='published' \
           AND (v.effective_start_at IS NULL OR v.effective_start_at<=now()) \
           AND (v.effective_end_at IS NULL OR v.effective_end_at>now()) \
         ORDER BY v.version DESC LIMIT 1 FOR SHARE OF p,v",
    )
    .bind(access_plan_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    if version.is_some() {
        Ok(())
    } else {
        Err(gql::BillingError::NotFound(format!(
            "enabled access plan {access_plan_id} with a published version"
        )))
    }
}

async fn ensure_project_membership(
    pool: &PgPool,
    user_id: i64,
    project_id: i64,
) -> Result<(), gql::BillingError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM user_projects up JOIN projects p ON p.id=up.project_id \
         WHERE up.user_id=$1 AND up.project_id=$2 AND p.status='active' AND p.deleted_at=0)",
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(storage)?;
    if exists {
        Ok(())
    } else {
        Err(gql::BillingError::Invalid(format!(
            "user {user_id} is not a member of active project {project_id}"
        )))
    }
}

fn parse_id(value: &str) -> Result<i64, gql::BillingError> {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .parse()
        .map_err(|_| gql::BillingError::Invalid(format!("invalid id {value:?}")))
}

fn parse_access_plan_ids(values: Vec<ID>) -> Result<Vec<i64>, gql::BillingError> {
    let values = values
        .into_iter()
        .map(|value| parse_id(value.as_str()))
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(values.into_iter().collect())
}

fn parse_positive_amount(value: &str) -> Result<i64, gql::BillingError> {
    let value = parse_nonnegative_amount(value)?;
    if value == 0 {
        Err(gql::BillingError::Invalid(
            "amount must be greater than zero".into(),
        ))
    } else {
        Ok(value)
    }
}

fn parse_nonnegative_amount(value: &str) -> Result<i64, gql::BillingError> {
    let decimal = Decimal::from_str(value.trim())
        .map_err(|_| gql::BillingError::Invalid("amount must be a decimal".into()))?;
    if decimal < Decimal::ZERO {
        return Err(gql::BillingError::Invalid(
            "amount cannot be negative".into(),
        ));
    }
    decimal_to_micros(decimal).map_err(|error| gql::BillingError::Invalid(error.into()))
}

fn parse_signed_amount(value: &str) -> Result<i64, gql::BillingError> {
    Decimal::from_str(value.trim())
        .map_err(|_| gql::BillingError::Invalid("amount must be a decimal".into()))
        .and_then(|decimal| {
            decimal_to_micros(decimal).map_err(|error| gql::BillingError::Invalid(error.into()))
        })
}

fn amount(value: i64) -> String {
    micros_to_decimal(value).normalize().to_string()
}

fn id(value: i64) -> ID {
    ID(value.to_string())
}

fn user_node_id(value: i64) -> ID {
    node_id("User", value)
}

fn project_node_id(value: i64) -> ID {
    node_id("Project", value)
}

fn node_id(kind: &str, value: i64) -> ID {
    ID(format!("gid://conduit/{kind}/{value}"))
}

fn storage(error: sqlx::Error) -> gql::BillingError {
    gql::BillingError::Storage(error.to_string())
}

fn nonempty(value: String, field: &str) -> Result<String, gql::BillingError> {
    let value = value.trim();
    if value.is_empty() {
        Err(gql::BillingError::Invalid(format!(
            "{field} cannot be empty"
        )))
    } else {
        Ok(value.to_string())
    }
}

fn interval_to_wire(value: gql::SubscriptionIntervalUnit) -> &'static str {
    match value {
        gql::SubscriptionIntervalUnit::Day => "day",
        gql::SubscriptionIntervalUnit::Month => "month",
        gql::SubscriptionIntervalUnit::Year => "year",
    }
}

fn interval_from_wire(value: &str) -> gql::SubscriptionIntervalUnit {
    match value {
        "day" => gql::SubscriptionIntervalUnit::Day,
        "year" => gql::SubscriptionIntervalUnit::Year,
        _ => gql::SubscriptionIntervalUnit::Month,
    }
}

fn rollover_to_wire(value: gql::RolloverMode) -> &'static str {
    match value {
        gql::RolloverMode::None => "none",
        gql::RolloverMode::Capped => "capped",
    }
}

fn quota_class_to_wire(value: gql::QuotaClass) -> &'static str {
    match value {
        gql::QuotaClass::General => "GENERAL",
        gql::QuotaClass::Dedicated => "DEDICATED",
    }
}

fn quota_class_from_wire(value: &str) -> gql::QuotaClass {
    if value == "DEDICATED" {
        gql::QuotaClass::Dedicated
    } else {
        gql::QuotaClass::General
    }
}

fn status_from_wire(value: &str) -> gql::BillingStatus {
    match value {
        "disabled" => gql::BillingStatus::Disabled,
        "archived" => gql::BillingStatus::Archived,
        _ => gql::BillingStatus::Enabled,
    }
}

fn status_to_wire(value: gql::BillingStatus) -> &'static str {
    match value {
        gql::BillingStatus::Enabled => "enabled",
        gql::BillingStatus::Disabled => "disabled",
        gql::BillingStatus::Archived => "archived",
    }
}

fn wire_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::billing::BillingServices as _;
    use sqlx::types::Json;

    async fn access_plan_with_model(
        pool: &PgPool,
        suffix: &str,
        label: &str,
    ) -> Result<(i64, i64), sqlx::Error> {
        let model_key = format!("pg-billing-{label}-{suffix}");
        let model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) \
             VALUES('billing-test',$1,$1,'','billing-test','{}'::jsonb,'{}'::jsonb,'enabled') \
             RETURNING id",
        )
        .bind(&model_key)
        .fetch_one(pool)
        .await?;
        let access_plan_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO access_plans(name,status,is_default,created_at,updated_at) \
             VALUES($1,'enabled',FALSE,now(),now()) RETURNING id",
        )
        .bind(format!("PG Billing {label} {suffix}"))
        .fetch_one(pool)
        .await?;
        let version_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO access_plan_versions \
             (access_plan_id,version,status,reference_id,effective_start_at,created_at,updated_at) \
             VALUES($1,1,'published',$2,now(),now(),now()) RETURNING id",
        )
        .bind(access_plan_id)
        .bind(format!("pg-billing-{label}-{suffix}"))
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) \
             VALUES($1,$2,now())",
        )
        .bind(version_id)
        .bind(model_id)
        .execute(pool)
        .await?;
        Ok((access_plan_id, model_id))
    }

    #[test]
    fn quota_rule_validation_rejects_ambiguous_scope_and_rollover_settings() {
        assert!(validate_quota_rules(Vec::new()).is_err());
        let general_with_scope = gql::SubscriptionQuotaRuleInput {
            id: None,
            name: "general".into(),
            quota_class: gql::QuotaClass::General,
            allowance: "1".into(),
            rollover_mode: Some(gql::RolloverMode::None),
            rollover_cap: None,
            carryover_days: None,
            access_plan_ids: vec![id(1)],
        };
        assert!(validate_quota_rules(vec![general_with_scope]).is_err());
        let dedicated_without_scope = gql::SubscriptionQuotaRuleInput {
            id: None,
            name: "dedicated".into(),
            quota_class: gql::QuotaClass::Dedicated,
            allowance: "1".into(),
            rollover_mode: Some(gql::RolloverMode::None),
            rollover_cap: None,
            carryover_days: None,
            access_plan_ids: Vec::new(),
        };
        assert!(validate_quota_rules(vec![dedicated_without_scope]).is_err());
        let capped_without_duration = gql::SubscriptionQuotaRuleInput {
            id: None,
            name: "carry".into(),
            quota_class: gql::QuotaClass::General,
            allowance: "1".into(),
            rollover_mode: Some(gql::RolloverMode::Capped),
            rollover_cap: Some("1".into()),
            carryover_days: None,
            access_plan_ids: Vec::new(),
        };
        assert!(validate_quota_rules(vec![capped_without_duration]).is_err());
        let valid = gql::SubscriptionQuotaRuleInput {
            id: None,
            name: "carry".into(),
            quota_class: gql::QuotaClass::General,
            allowance: "1".into(),
            rollover_mode: Some(gql::RolloverMode::Capped),
            rollover_cap: Some("1".into()),
            carryover_days: Some(30),
            access_plan_ids: Vec::new(),
        };
        let validated = validate_quota_rules(vec![valid]).expect("valid quota rule");
        assert_eq!(validated[0].amount_micros, 1_000_000);
        assert_eq!(
            validated[0].carry_duration_seconds,
            Some(30 * SECONDS_PER_DAY)
        );
    }

    #[tokio::test]
    async fn postgres_subscription_reads_one_snapshot_and_excludes_draining_buckets_from_summary_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','Snapshot','Billing',FALSE,$2) RETURNING id",
        )
        .bind(format!("pg-billing-snapshot-{suffix}@example.com"))
        .bind(Json(Vec::<String>::new()))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'billing snapshot integration','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Billing Snapshot {suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) \
             VALUES($1,$2,TRUE,$3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(Vec::<String>::new()))
        .execute(&pool)
        .await?;
        let (access_plan_id, _) = access_plan_with_model(&pool, &suffix, "snapshot").await?;
        let adapter = PgBillingAdapter::new(pool.clone());
        let plan = adapter
            .create_subscription_plan(gql::CreateSubscriptionPlanInput {
                name: format!("PG snapshot {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: Some(1),
                access_plan_ids: vec![id(access_plan_id)],
                quota_rules: vec![
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "Current general".into(),
                        quota_class: gql::QuotaClass::General,
                        allowance: "10".into(),
                        rollover_mode: Some(gql::RolloverMode::None),
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: Vec::new(),
                    },
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "Old dedicated".into(),
                        quota_class: gql::QuotaClass::Dedicated,
                        allowance: "20".into(),
                        rollover_mode: Some(gql::RolloverMode::None),
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: vec![id(access_plan_id)],
                    },
                ],
            })
            .await?;
        let assigned = adapter
            .assign_user_subscription(gql::AssignUserSubscriptionInput {
                user_id: id(user_id),
                plan_id: plan.id,
                project_id: id(project_id),
                idempotency_key: format!("pg-billing-snapshot-assignment-{suffix}"),
                auto_renew: Some(false),
                interval_unit: None,
                interval_count: None,
            })
            .await?;
        let subscription_id = parse_id(assigned.id.as_str())?;
        let bucket_rows = sqlx::query(
            "SELECT b.id,r.rule_name FROM subscription_allowance_buckets b \
             JOIN user_subscription_quota_rule_snapshots r ON r.id=b.quota_rule_snapshot_id \
             WHERE b.subscription_id=$1",
        )
        .bind(subscription_id)
        .fetch_all(&pool)
        .await?;
        let current_bucket_id = bucket_rows
            .iter()
            .find(|row| row.get::<String, _>("rule_name") == "Current general")
            .map(|row| row.get::<i64, _>("id"))
            .expect("current bucket");
        let old_bucket_id = bucket_rows
            .iter()
            .find(|row| row.get::<String, _>("rule_name") == "Old dedicated")
            .map(|row| row.get::<i64, _>("id"))
            .expect("old bucket");
        sqlx::query(
            "UPDATE subscription_allowance_buckets \
             SET consumed_micros=2000000,reserved_micros=1000000 WHERE id=$1",
        )
        .bind(current_bucket_id)
        .execute(&pool)
        .await?;
        let old_start = Utc::now() - Duration::days(2);
        let old_end = Utc::now() - Duration::days(1);
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET period_start=$1,period_end=$2,expires_at=$2, \
             consumed_micros=4000000,reserved_micros=3000000,status='draining' WHERE id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(old_bucket_id)
        .execute(&pool)
        .await?;

        let mut tx = adapter.begin_read_snapshot().await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW transaction_isolation")
                .fetch_one(&mut *tx)
                .await?,
            "repeatable read"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SHOW transaction_read_only")
                .fetch_one(&mut *tx)
                .await?,
            "on"
        );
        let initial = adapter.subscription_tx(&mut tx, subscription_id).await?;
        assert_eq!(initial.granted_allowance, "10");
        assert_eq!(initial.consumed_allowance, "2");
        assert_eq!(initial.reserved_allowance, "1");
        assert_eq!(initial.remaining_allowance, "7");
        assert_eq!(initial.general_remaining_allowance, "7");
        assert_eq!(initial.dedicated_remaining_allowance, "0");
        assert!(initial.allowance_buckets.iter().any(|bucket| {
            bucket.id == id(old_bucket_id)
                && bucket.status == "draining"
                && bucket.remaining_allowance == "0"
        }));

        sqlx::query(
            "UPDATE subscription_allowance_buckets SET consumed_micros=3000000 WHERE id=$1",
        )
        .bind(current_bucket_id)
        .execute(&pool)
        .await?;
        let repeated = adapter.subscription_tx(&mut tx, subscription_id).await?;
        assert_eq!(repeated.consumed_allowance, "2");
        assert_eq!(repeated.remaining_allowance, "7");
        tx.commit().await?;

        let fresh = adapter.subscription(subscription_id).await?;
        assert_eq!(fresh.granted_allowance, "10");
        assert_eq!(fresh.consumed_allowance, "3");
        assert_eq!(fresh.reserved_allowance, "1");
        assert_eq!(fresh.remaining_allowance, "6");

        sqlx::query(
            "UPDATE subscription_allowance_buckets SET reserved_micros=0,status='active' WHERE id=$1",
        )
        .bind(old_bucket_id)
        .execute(&pool)
        .await?;
        let expired = adapter.subscription(subscription_id).await?;
        assert!(expired.allowance_buckets.iter().any(|bucket| {
            bucket.id == id(old_bucket_id)
                && bucket.status == "expired"
                && bucket.remaining_allowance == "0"
        }));
        assert_eq!(expired.granted_allowance, "10");
        assert_eq!(expired.consumed_allowance, "3");
        assert_eq!(expired.reserved_allowance, "1");
        assert_eq!(expired.remaining_allowance, "6");
        Ok(())
    }

    #[tokio::test]
    async fn postgres_subscription_assignment_is_idempotent_and_stackable_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','Assignment','Idempotency',FALSE,$2) RETURNING id",
        )
        .bind(format!("pg-assignment-{suffix}@example.com"))
        .bind(Json(Vec::<String>::new()))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'assignment idempotency integration','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Assignment {suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) \
             VALUES($1,$2,TRUE,$3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(Vec::<String>::new()))
        .execute(&pool)
        .await?;
        let (access_plan_id, _) = access_plan_with_model(&pool, &suffix, "assignment").await?;
        let adapter = PgBillingAdapter::new(pool.clone());
        let plan = adapter
            .create_subscription_plan(gql::CreateSubscriptionPlanInput {
                name: format!("PG assignment {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: Some(1),
                access_plan_ids: vec![id(access_plan_id)],
                quota_rules: vec![gql::SubscriptionQuotaRuleInput {
                    id: None,
                    name: "General assignment allowance".into(),
                    quota_class: gql::QuotaClass::General,
                    allowance: "10".into(),
                    rollover_mode: Some(gql::RolloverMode::None),
                    rollover_cap: None,
                    carryover_days: None,
                    access_plan_ids: Vec::new(),
                }],
            })
            .await?;
        let plan_id = parse_id(plan.id.as_str())?;
        let sequential_key = format!("pg-assignment-sequential-{suffix}");
        let base_input = gql::AssignUserSubscriptionInput {
            user_id: id(user_id),
            plan_id: plan.id.clone(),
            project_id: id(project_id),
            idempotency_key: sequential_key.clone(),
            auto_renew: None,
            interval_unit: None,
            interval_count: None,
        };

        let mut padded_input = base_input.clone();
        padded_input.idempotency_key = format!("  {sequential_key}  ");
        let first = adapter.assign_user_subscription(padded_input).await?;
        let first_id = parse_id(first.id.as_str())?;
        let mut semantic_replay = base_input.clone();
        semantic_replay.auto_renew = Some(true);
        let replayed = adapter.assign_user_subscription(semantic_replay).await?;
        assert_eq!(replayed.id, first.id);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_subscriptions WHERE assignment_key=$1",
            )
            .bind(&sequential_key)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, Value>(
                "SELECT assignment_request_snapshot FROM user_subscriptions WHERE id=$1",
            )
            .bind(first_id)
            .fetch_one(&pool)
            .await?,
            json!({
                "schemaVersion": 1,
                "userID": user_id,
                "planID": plan_id,
                "projectID": project_id,
                "autoRenew": true,
                "intervalUnit": null,
                "intervalCount": null,
            })
        );

        let mut conflicting = base_input.clone();
        // An explicit override remains distinct even when it currently equals
        // the plan default; the nullable request shape is part of the snapshot.
        conflicting.interval_count = Some(1);
        match adapter.assign_user_subscription(conflicting).await {
            Err(gql::BillingError::Invalid(message)) => assert!(
                message.contains("already used for a different subscription assignment"),
                "unexpected conflict message: {message}"
            ),
            other => panic!("expected assignment-key conflict, got {other:?}"),
        }
        let mut empty_key = base_input.clone();
        empty_key.idempotency_key = " \t ".into();
        match adapter.assign_user_subscription(empty_key).await {
            Err(gql::BillingError::Invalid(message)) => {
                assert!(message.contains("idempotencyKey cannot be empty"));
            }
            other => panic!("expected empty-key rejection, got {other:?}"),
        }

        let stacking_key = format!("pg-assignment-stacking-{suffix}");
        let mut stacking_input = base_input.clone();
        stacking_input.idempotency_key = stacking_key;
        let stacked = adapter.assign_user_subscription(stacking_input).await?;
        assert_ne!(stacked.id, first.id);

        let concurrent_key = format!("pg-assignment-concurrent-{suffix}");
        let mut concurrent_input = base_input.clone();
        concurrent_input.idempotency_key = concurrent_key.clone();
        let mut assignments = Vec::new();
        for _ in 0..8 {
            let adapter = adapter.clone();
            let input = concurrent_input.clone();
            assignments.push(tokio::spawn(async move {
                adapter.assign_user_subscription(input).await
            }));
        }
        let mut concurrent_ids = BTreeSet::new();
        for assignment in assignments {
            concurrent_ids.insert(parse_id(assignment.await??.id.as_str())?);
        }
        assert_eq!(concurrent_ids.len(), 1);
        let concurrent_id = *concurrent_ids.iter().next().expect("concurrent assignment");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_subscriptions WHERE assignment_key=$1",
            )
            .bind(&concurrent_key)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM subscription_allowance_buckets WHERE subscription_id=$1",
            )
            .bind(concurrent_id)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM user_subscriptions WHERE user_id=$1 AND plan_id=$2",
            )
            .bind(user_id)
            .bind(plan_id)
            .fetch_one(&pool)
            .await?,
            3
        );

        sqlx::query("UPDATE subscription_plans SET status='disabled' WHERE id=$1")
            .bind(plan_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_projects WHERE user_id=$1 AND project_id=$2")
            .bind(user_id)
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE user_subscriptions SET auto_renew=FALSE WHERE id=$1")
            .bind(first_id)
            .execute(&pool)
            .await?;
        let mut disabled_replay = base_input;
        disabled_replay.auto_renew = Some(true);
        let disabled_replay = adapter.assign_user_subscription(disabled_replay).await?;
        assert_eq!(disabled_replay.id, first.id);
        assert!(!disabled_replay.auto_renew);
        assert_eq!(disabled_replay.plan.status, gql::BillingStatus::Disabled);
        isolated.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_billing_covers_multi_group_stacking_wallet_idempotency_and_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','PG','Billing',FALSE,$2) RETURNING id",
        )
        .bind(format!("pg-billing-{suffix}@example.com"))
        .bind(Json(Vec::<String>::new()))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'billing integration','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Billing {suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) \
             VALUES($1,$2,TRUE,$3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(Vec::<String>::new()))
        .execute(&pool)
        .await?;
        let (group_a, model_a) = access_plan_with_model(&pool, &suffix, "a").await?;
        let (group_b, model_b) = access_plan_with_model(&pool, &suffix, "b").await?;
        let adapter = PgBillingAdapter::new(pool.clone());

        let plan_multi = adapter
            .create_subscription_plan(gql::CreateSubscriptionPlanInput {
                name: format!("PG multi {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: Some(1),
                access_plan_ids: vec![id(group_a), id(group_b)],
                quota_rules: vec![
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "General".into(),
                        quota_class: gql::QuotaClass::General,
                        allowance: "20".into(),
                        rollover_mode: Some(gql::RolloverMode::None),
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: Vec::new(),
                    },
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "Dedicated A+B".into(),
                        quota_class: gql::QuotaClass::Dedicated,
                        allowance: "30".into(),
                        rollover_mode: Some(gql::RolloverMode::None),
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: vec![id(group_a), id(group_b)],
                    },
                ],
            })
            .await?;
        assert_eq!(plan_multi.currency, STATION_CREDIT_CODE);
        assert_eq!(plan_multi.access_plans.len(), 2);
        assert_eq!(plan_multi.quota_rules.len(), 2);
        let multi_plan_id = parse_id(plan_multi.id.as_str())?;
        let rule_keys_before = sqlx::query(
            "SELECT id,rule_key FROM subscription_quota_rules \
             WHERE subscription_plan_id=$1 ORDER BY id",
        )
        .bind(multi_plan_id)
        .fetch_all(&pool)
        .await?
        .into_iter()
        .map(|row| (row.get::<i64, _>("id"), row.get::<String, _>("rule_key")))
        .collect::<Vec<_>>();
        let plan_stack = adapter
            .create_subscription_plan(gql::CreateSubscriptionPlanInput {
                name: format!("PG stack {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: Some(1),
                access_plan_ids: vec![id(group_b)],
                quota_rules: vec![gql::SubscriptionQuotaRuleInput {
                    id: None,
                    name: "Dedicated B".into(),
                    quota_class: gql::QuotaClass::Dedicated,
                    allowance: "10".into(),
                    rollover_mode: Some(gql::RolloverMode::None),
                    rollover_cap: None,
                    carryover_days: None,
                    access_plan_ids: vec![id(group_b)],
                }],
            })
            .await?;
        let first = adapter
            .assign_user_subscription(gql::AssignUserSubscriptionInput {
                user_id: id(user_id),
                plan_id: plan_multi.id.clone(),
                project_id: id(project_id),
                idempotency_key: format!("pg-billing-multi-assignment-{suffix}"),
                auto_renew: Some(true),
                interval_unit: None,
                interval_count: None,
            })
            .await?;
        assert_eq!(first.granted_access_plans.len(), 2);
        assert_eq!(
            first
                .granted_model_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                format!("pg-billing-a-{suffix}"),
                format!("pg-billing-b-{suffix}")
            ])
        );
        let second = adapter
            .assign_user_subscription(gql::AssignUserSubscriptionInput {
                user_id: id(user_id),
                plan_id: plan_stack.id,
                project_id: id(project_id),
                idempotency_key: format!("pg-billing-stack-assignment-{suffix}"),
                auto_renew: Some(false),
                interval_unit: None,
                interval_count: None,
            })
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM project_access_grants WHERE project_id=$1 \
                 AND source_type='subscription' AND status='active'"
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            3
        );
        let stacked = adapter.project_balance(&project_id.to_string()).await?;
        assert_eq!(stacked.subscription_balance, "60");

        // Product edits are atomic and affect new assignments only. Existing
        // subscriptions retain their commercial and entitlement snapshots.
        let updated_plan = adapter
            .update_subscription_plan(gql::UpdateSubscriptionPlanInput {
                id: plan_multi.id.clone(),
                name: format!("PG multi updated {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: 1,
                access_plan_ids: vec![id(group_a)],
                quota_rules: plan_multi
                    .quota_rules
                    .iter()
                    .map(|rule| gql::SubscriptionQuotaRuleInput {
                        id: Some(rule.id.clone()),
                        name: rule.name.clone(),
                        quota_class: rule.quota_class,
                        allowance: if rule.quota_class == gql::QuotaClass::Dedicated {
                            "35".into()
                        } else {
                            "20".into()
                        },
                        rollover_mode: Some(gql::RolloverMode::None),
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: if rule.quota_class == gql::QuotaClass::Dedicated {
                            vec![id(group_a)]
                        } else {
                            Vec::new()
                        },
                    })
                    .collect(),
                status: gql::BillingStatus::Enabled,
            })
            .await?;
        assert_eq!(updated_plan.access_plans.len(), 1);
        let rule_keys_after = sqlx::query(
            "SELECT id,rule_key FROM subscription_quota_rules \
             WHERE subscription_plan_id=$1 ORDER BY id",
        )
        .bind(multi_plan_id)
        .fetch_all(&pool)
        .await?
        .into_iter()
        .map(|row| (row.get::<i64, _>("id"), row.get::<String, _>("rule_key")))
        .collect::<Vec<_>>();
        assert_eq!(rule_keys_after, rule_keys_before);
        let snapshotted = adapter.subscription(parse_id(first.id.as_str())?).await?;
        assert_eq!(snapshotted.granted_access_plans.len(), 2);
        let dedicated_snapshot = snapshotted
            .plan
            .quota_rules
            .iter()
            .find(|rule| rule.quota_class == gql::QuotaClass::Dedicated)
            .expect("dedicated rule snapshot");
        assert_eq!(dedicated_snapshot.allowance, "30");
        assert_eq!(dedicated_snapshot.access_plans.len(), 2);
        assert_eq!(
            adapter
                .refresh_subscription_allowance(second.id.as_str())
                .await?
                .remaining_allowance,
            "10"
        );

        let idempotency_key = format!("pg-wallet-{suffix}");
        let mut grants = Vec::new();
        for _ in 0..8 {
            let adapter = adapter.clone();
            let key = idempotency_key.clone();
            grants.push(tokio::spawn(async move {
                adapter
                    .grant_project_credit(gql::GrantProjectCreditInput {
                        project_id: id(project_id),
                        amount: "25".into(),
                        description: Some("concurrent grant".into()),
                        idempotency_key: key,
                    })
                    .await
            }));
        }
        for grant in grants {
            let balance = grant.await??;
            assert_eq!(balance.credit_balance, "25");
            assert_eq!(balance.currency, STATION_CREDIT_CODE);
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM project_credit_ledger_entries WHERE idempotency_key=$1"
            )
            .bind(&idempotency_key)
            .fetch_one(&pool)
            .await?,
            1
        );
        assert!(
            adapter
                .grant_project_credit(gql::GrantProjectCreditInput {
                    project_id: id(project_id),
                    amount: "26".into(),
                    description: None,
                    idempotency_key: idempotency_key.clone(),
                })
                .await
                .is_err()
        );

        // Auto renewal refreshes allowance while holding the subscription row
        // lock, but it must retain the entitlement versions captured when the
        // subscription was assigned.
        let first_id = parse_id(first.id.as_str())?;
        let old_start = Utc::now() - Duration::days(2);
        let old_end = Utc::now() - Duration::days(1);
        sqlx::query(
            "UPDATE user_subscriptions SET current_period_start=$1,current_period_end=$2 WHERE id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(first_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET period_start=$1,period_end=$2,expires_at=$2 \
             WHERE subscription_id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(first_id)
        .execute(&pool)
        .await?;
        assert_eq!(adapter.process_due_subscriptions().await?, 1);
        let automatically_renewed = adapter.subscription(first_id).await?;
        assert_eq!(
            automatically_renewed.plan.name,
            format!("PG multi updated {suffix}")
        );
        assert_eq!(automatically_renewed.plan.allowance, "50");
        assert_eq!(automatically_renewed.remaining_allowance, "50");
        assert_eq!(automatically_renewed.granted_access_plans.len(), 2);

        let paused = adapter.pause_user_subscription(first.id.as_str()).await?;
        assert_eq!(paused.status, "paused");
        assert!(paused.granted_access_plans.is_empty());
        assert_eq!(
            adapter
                .project_balance(&project_id.to_string())
                .await?
                .subscription_balance,
            "10"
        );
        let resumed = adapter.resume_user_subscription(first.id.as_str()).await?;
        assert_eq!(resumed.granted_access_plans.len(), 2);
        let canceled = adapter.cancel_user_subscription(first.id.as_str()).await?;
        assert_eq!(canceled.status, "cancel_pending");
        assert!(!canceled.auto_renew);

        let old_start = Utc::now() - Duration::days(2);
        let old_end = Utc::now() - Duration::days(1);
        let second_id = parse_id(second.id.as_str())?;
        sqlx::query(
            "UPDATE user_subscriptions SET current_period_start=$1,current_period_end=$2 WHERE id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(second_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET period_start=$1,period_end=$2,expires_at=$2 \
             WHERE subscription_id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(second_id)
        .execute(&pool)
        .await?;
        assert_eq!(adapter.process_due_subscriptions().await?, 1);
        assert_eq!(adapter.subscription(second_id).await?.status, "expired");
        let renewed = adapter.renew_user_subscription(second.id.as_str()).await?;
        assert_eq!(renewed.status, "active");
        assert_eq!(renewed.remaining_allowance, "10");
        assert_eq!(renewed.granted_access_plans.len(), 1);

        let self_service = adapter
            .user_project_subscriptions(&user_id.to_string(), &project_id.to_string())
            .await?;
        assert_eq!(self_service.len(), 2);
        adapter
            .record_commercial_operation_audit(gql::CommercialOperationAudit {
                actor_type: "user".into(),
                actor_id: Some(user_id.to_string()),
                operation: "postgres_billing_test".into(),
                target_project_id: Some(project_id.to_string()),
                target_user_id: Some(user_id.to_string()),
                amount: Some("25".into()),
                currency: Some(STATION_CREDIT_CODE.into()),
                plan_id: Some(plan_multi.id.to_string()),
                plan_name: None,
                subscription_id: Some(second.id.to_string()),
                idempotency_key: Some(idempotency_key),
                result: "success".into(),
                error_message: None,
            })
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM commercial_operation_audits \
                 WHERE operation='postgres_billing_test' AND target_project_id=$1"
            )
            .bind(project_id.to_string())
            .fetch_one(&pool)
            .await?,
            1
        );
        // Keep the model IDs live so PostgreSQL actually checks both snapshots.
        assert_ne!(model_a, model_b);
        Ok(())
    }

    #[tokio::test]
    async fn postgres_billing_issues_independent_rule_and_carry_buckets_and_preserves_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','Quota','Lifecycle',FALSE,$2) RETURNING id",
        )
        .bind(format!("pg-quota-{suffix}@example.com"))
        .bind(Json(Vec::<String>::new()))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'quota lifecycle integration','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Quota {suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES($1,$2,TRUE,$3)",
        )
        .bind(user_id)
        .bind(project_id)
        .bind(Json(Vec::<String>::new()))
        .execute(&pool)
        .await?;
        let (access_plan_id, _) = access_plan_with_model(&pool, &suffix, "rollover").await?;
        let adapter = PgBillingAdapter::new(pool.clone());
        let plan = adapter
            .create_subscription_plan(gql::CreateSubscriptionPlanInput {
                name: format!("PG rollover {suffix}"),
                interval_unit: gql::SubscriptionIntervalUnit::Month,
                interval_count: Some(1),
                access_plan_ids: vec![id(access_plan_id)],
                quota_rules: vec![
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "General carry".into(),
                        quota_class: gql::QuotaClass::General,
                        allowance: "100".into(),
                        rollover_mode: Some(gql::RolloverMode::Capped),
                        rollover_cap: Some("30".into()),
                        carryover_days: Some(10),
                        access_plan_ids: Vec::new(),
                    },
                    gql::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "Dedicated carry".into(),
                        quota_class: gql::QuotaClass::Dedicated,
                        allowance: "50".into(),
                        rollover_mode: Some(gql::RolloverMode::Capped),
                        rollover_cap: Some("20".into()),
                        carryover_days: Some(5),
                        access_plan_ids: vec![id(access_plan_id)],
                    },
                ],
            })
            .await?;
        assert_eq!(plan.allowance, "150");
        assert_eq!(plan.rollover_cap.as_deref(), Some("50"));
        let assigned = adapter
            .assign_user_subscription(gql::AssignUserSubscriptionInput {
                user_id: id(user_id),
                plan_id: plan.id,
                project_id: id(project_id),
                idempotency_key: format!("pg-billing-rollover-assignment-{suffix}"),
                auto_renew: Some(true),
                interval_unit: Some(gql::SubscriptionIntervalUnit::Day),
                interval_count: Some(2),
            })
            .await?;
        assert_eq!(assigned.allowance_buckets.len(), 2);
        assert_eq!(assigned.interval_unit, gql::SubscriptionIntervalUnit::Day);
        assert_eq!(assigned.interval_count, 2);
        assert_eq!(
            assigned.plan.interval_unit,
            gql::SubscriptionIntervalUnit::Day
        );
        assert_eq!(assigned.plan.interval_count, 2);
        assert_eq!(assigned.general_remaining_allowance, "100");
        assert_eq!(assigned.dedicated_remaining_allowance, "50");
        let subscription_id = parse_id(assigned.id.as_str())?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET consumed_micros=60000000,reserved_micros=10000000 \
             WHERE subscription_id=$1 AND quota_class='GENERAL' AND source_bucket_id IS NULL",
        )
        .bind(subscription_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET consumed_micros=10000000,reserved_micros=15000000 \
             WHERE subscription_id=$1 AND quota_class='DEDICATED' AND source_bucket_id IS NULL",
        )
        .bind(subscription_id)
        .execute(&pool)
        .await?;
        let old_start = Utc::now() - Duration::days(2);
        let old_end = Utc::now() - Duration::days(1);
        sqlx::query(
            "UPDATE user_subscriptions SET current_period_start=$1,current_period_end=$2 WHERE id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(subscription_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE subscription_allowance_buckets SET period_start=$1,period_end=$2,expires_at=$2 \
             WHERE subscription_id=$3",
        )
        .bind(old_start)
        .bind(old_end)
        .bind(subscription_id)
        .execute(&pool)
        .await?;

        assert_eq!(adapter.process_due_subscriptions().await?, 1);
        let renewed = adapter.subscription(subscription_id).await?;
        assert_eq!(renewed.interval_unit, gql::SubscriptionIntervalUnit::Day);
        assert_eq!(renewed.interval_count, 2);
        let renewed_start = DateTime::parse_from_rfc3339(&renewed.current_period_start)?;
        let renewed_end = DateTime::parse_from_rfc3339(&renewed.current_period_end)?;
        assert_eq!(renewed_end - renewed_start, Duration::days(2));
        assert_eq!(renewed.allowance_buckets.len(), 6);
        assert_eq!(renewed.general_remaining_allowance, "130");
        assert_eq!(renewed.dedicated_remaining_allowance, "70");
        assert_eq!(renewed.remaining_allowance, "200");
        assert_eq!(
            renewed
                .allowance_buckets
                .iter()
                .filter(|bucket| bucket.source_type == "CARRYOVER")
                .count(),
            2
        );
        assert_eq!(
            renewed
                .allowance_buckets
                .iter()
                .filter(|bucket| bucket.status == "draining")
                .count(),
            2
        );
        assert!(
            renewed
                .allowance_buckets
                .iter()
                .filter(|bucket| bucket.status == "draining")
                .all(|bucket| bucket.remaining_allowance == "0")
        );
        let carry_expiries = sqlx::query(
            "SELECT quota_class,expires_at FROM subscription_allowance_buckets \
             WHERE subscription_id=$1 AND source_bucket_id IS NOT NULL ORDER BY quota_class",
        )
        .bind(subscription_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(carry_expiries.len(), 2);
        for carry in carry_expiries {
            let expected = if carry.get::<String, _>("quota_class") == "GENERAL" {
                old_end + Duration::days(10)
            } else {
                old_end + Duration::days(5)
            };
            assert_eq!(
                carry
                    .get::<DateTime<Utc>, _>("expires_at")
                    .timestamp_micros(),
                expected.timestamp_micros()
            );
        }

        let paused = adapter
            .pause_user_subscription(assigned.id.as_str())
            .await?;
        assert_eq!(paused.status, "paused");
        assert!(
            paused
                .allowance_buckets
                .iter()
                .filter(|bucket| bucket.status != "draining")
                .all(|bucket| bucket.status == "paused")
        );
        assert_eq!(
            adapter
                .project_balance(&project_id.to_string())
                .await?
                .subscription_balance,
            "0"
        );
        let resumed = adapter
            .resume_user_subscription(assigned.id.as_str())
            .await?;
        assert_eq!(resumed.status, "active");
        assert_eq!(resumed.remaining_allowance, "200");
        let cancel_pending = adapter
            .cancel_user_subscription(assigned.id.as_str())
            .await?;
        assert_eq!(cancel_pending.status, "cancel_pending");
        assert!(!cancel_pending.auto_renew);
        assert!(
            cancel_pending
                .allowance_buckets
                .iter()
                .any(|bucket| bucket.status == "active")
        );

        sqlx::query("UPDATE user_subscriptions SET current_period_end=$1 WHERE id=$2")
            .bind(Utc::now() - Duration::days(1))
            .bind(subscription_id)
            .execute(&pool)
            .await?;
        assert_eq!(adapter.process_due_subscriptions().await?, 1);
        let expired = adapter.subscription(subscription_id).await?;
        assert_eq!(expired.status, "expired");
        assert_eq!(expired.remaining_allowance, "0");
        let explicitly_renewed = adapter
            .renew_user_subscription(assigned.id.as_str())
            .await?;
        assert_eq!(explicitly_renewed.status, "active");
        assert_eq!(explicitly_renewed.remaining_allowance, "150");
        assert_eq!(
            explicitly_renewed
                .allowance_buckets
                .iter()
                .filter(|bucket| bucket.source_type == "CURRENT" && bucket.status == "active")
                .count(),
            2
        );
        Ok(())
    }
}

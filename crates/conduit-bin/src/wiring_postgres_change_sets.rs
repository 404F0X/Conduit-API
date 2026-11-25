#[path = "wiring_postgres_change_packages.rs"]
pub(crate) mod packages;

#[derive(Clone, Debug)]
pub(crate) struct OperationContext {
    pub operation_id: uuid::Uuid,
    pub origin: conduit_admin_graphql::change_package::OperationOrigin,
    pub actor_user_id: Option<i64>,
}
impl OperationContext {
    pub(crate) fn automatic() -> Self {
        Self {
            operation_id: uuid::Uuid::new_v4(),
            origin: conduit_admin_graphql::change_package::OperationOrigin::AutomaticSync,
            actor_user_id: None,
        }
    }
}
use async_graphql::ID;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::change_set as gql;
use conduit_core::objects::pricing::ModelPrice;
use conduit_db::repo::channel_model_price_repo::VERSION_STATUS_ACTIVE;
use serde_json::{Value, json};
use sqlx::{Acquire, FromRow, PgPool, Postgres, Row, Transaction, types::Json as SqlJson};

const CHANGE_SET_COLUMNS: &str = "id,kind,scope_type,scope_id,title,status,base_revision,\
source_revision,applied_target_type,applied_target_id,validation_error,created_by,submitted_by,\
reviewed_by,review_note,created_at,updated_at,submitted_at,reviewed_at,applied_at";
const CHANGE_SET_ITEM_COLUMNS: &str = "id,change_set_id,item_key,action,before_snapshot,\
after_snapshot,source_snapshot,validation_error,created_at,updated_at";

#[derive(Debug, FromRow)]
struct ChangeSetRow {
    id: i64,
    kind: String,
    scope_type: String,
    scope_id: String,
    title: String,
    status: String,
    base_revision: String,
    source_revision: String,
    applied_target_type: Option<String>,
    applied_target_id: Option<String>,
    validation_error: Option<String>,
    created_by: Option<i64>,
    submitted_by: Option<i64>,
    reviewed_by: Option<i64>,
    review_note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    submitted_at: Option<DateTime<Utc>>,
    reviewed_at: Option<DateTime<Utc>>,
    applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, FromRow)]
struct ChangeSetItemRow {
    id: i64,
    change_set_id: i64,
    item_key: String,
    action: String,
    before_snapshot: Option<SqlJson<Value>>,
    after_snapshot: Option<SqlJson<Value>>,
    source_snapshot: Option<SqlJson<Value>>,
    validation_error: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct ChangeSetEventRow {
    id: i64,
    change_set_id: i64,
    event_type: String,
    actor_type: String,
    actor_id: Option<i64>,
    detail: SqlJson<Value>,
    created_at: DateTime<Utc>,
}

pub(crate) struct PgChangeSetAdapter {
    pool: PgPool,
}

impl PgChangeSetAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stage_duplicated_provider_prices(
    tx: &mut Transaction<'_, Postgres>,
    actor_user_id: Option<i64>,
    source_channel_id: i64,
    target_channel_id: i64,
    target_channel_name: &str,
    source_prices: &[conduit_db::row::ChannelModelPriceRow],
    accounting_currency: &str,
    accounting_settings_version: u64,
    now: DateTime<Utc>,
) -> Result<Option<i64>, gql::ChangeSetError> {
    if source_prices.is_empty() {
        return Ok(None);
    }
    if let Some(price) = source_prices.iter().find(|price| {
        !price
            .currency_code
            .eq_ignore_ascii_case(accounting_currency)
    }) {
        return Err(gql::ChangeSetError::Invalid(format!(
            "source procurement price for model {} uses {}, expected accounting currency {accounting_currency}",
            price.model_id, price.currency_code
        )));
    }
    for price in source_prices {
        let parsed: ModelPrice = serde_json::from_value(price.price.clone()).map_err(|error| {
            gql::ChangeSetError::Invalid(format!(
                "source procurement price for model {} is invalid: {error}",
                price.model_id
            ))
        })?;
        crate::wiring::validate_model_price(&parsed).map_err(|error| {
            gql::ChangeSetError::Invalid(format!(
                "source procurement price for model {} is invalid: {error}",
                price.model_id
            ))
        })?;
        if parsed.items.is_empty() {
            return Err(gql::ChangeSetError::Invalid(format!(
                "source procurement price for model {} is empty",
                price.model_id
            )));
        }
    }

    let edit_base = packages::provider_base_in_tx(tx, target_channel_id).await?;
    let change_set_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO change_sets \
         (operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) \
         VALUES($6,'provider_price','channel',$1,$2,'draft',$7,$3,$4,$5,$5) RETURNING id",
    )
    .bind(target_channel_id.to_string())
    .bind(format!("Duplicated procurement prices: {target_channel_name}"))
    .bind(accounting_settings_version.to_string())
    .bind(actor_user_id)
    .bind(now)
    .bind(uuid::Uuid::new_v4())
    .bind(edit_base.revision)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage_error)?;
    let source = json!({
        "source": "manual",
        "origin": "CHANNEL_IMPORT",
        "sourceChannelID": source_channel_id,
        "accountingCurrency": accounting_currency,
        "accountingSettingsVersion": accounting_settings_version,
    });
    for price in source_prices {
        sqlx::query(
            "INSERT INTO change_set_items \
             (change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES($1,$2,'create',NULL,$3,$4,$5,$5)",
        )
        .bind(change_set_id)
        .bind(&price.model_id)
        .bind(SqlJson(price.price.clone()))
        .bind(SqlJson(source.clone()))
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(storage_error)?;
    }
    insert_event(
        tx,
        change_set_id,
        "created",
        if actor_user_id.is_some() {
            "user"
        } else {
            "system"
        },
        actor_user_id,
        json!({
            "source": "channel_duplicate",
            "sourceChannelID": source_channel_id,
            "itemCount": source_prices.len(),
        }),
        now,
    )
    .await?;
    Ok(Some(change_set_id))
}

pub(crate) async fn publish_copied_provider_prices(
    tx: &mut Transaction<'_, Postgres>,
    actor: i64,
    parent: Option<i64>,
    source: i64,
    target: i64,
    name: &str,
) -> Result<(), String> {
    crate::wiring::lock_accounting_currency_price_writes(tx)
        .await
        .map_err(|_| "price settings lock failed")?;
    let settings=crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx).await.map_err(|_|"price settings unavailable")?;
    let prices = conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(tx, source)
        .await
        .map_err(|_| "source prices unavailable")?;
    let now = Utc::now();
    let Some(id) = stage_duplicated_provider_prices(
        tx,
        Some(actor),
        source,
        target,
        name,
        &prices,
        &settings.currency,
        settings.version,
        now,
    )
    .await
    .map_err(|_| "price copy rejected")?
    else {
        return Ok(());
    };
    crate::wiring_governance::authorize(tx, actor, "write_channels").await?;
    crate::wiring_governance::authorize(tx, actor, "write_commercialization").await?;
    let cs = lock_change_set(tx, id)
        .await
        .map_err(|_| "price copy unavailable")?;
    let policy = crate::wiring_governance::lock_policy(tx).await?;
    if parent.is_none() && policy.requires("provider_price") {
        return Ok(());
    }
    let (kind, target) = apply_provider_price(tx, &cs, actor)
        .await
        .map_err(|_| "price copy publication rejected")?;
    sqlx::query("UPDATE change_sets SET status='applied',applied_at=$2,published_at=$2,published_by=$3,applied_target_type=$4,applied_target_id=$5,updated_at=$2 WHERE id=$1").bind(id).bind(now).bind(actor).bind(kind).bind(target).execute(&mut **tx).await.map_err(|_|"price copy receipt failed")?;
    insert_event(tx,id,"published","user",Some(actor),json!({"parentChangeSetID":parent,"approvalRequired":policy.requires("provider_price"),"policyRevision":policy.revision}),now).await.map_err(|_|"price copy audit failed")?;
    Ok(())
}

#[async_trait]
impl gql::ChangeSetServices for PgChangeSetAdapter {
    async fn change_sets(
        &self,
        kind: Option<gql::ChangeSetKind>,
        status: Option<gql::ChangeSetStatus>,
        scope_type: Option<String>,
        scope_id: Option<String>,
        limit: i32,
    ) -> Result<Vec<gql::ChangeSet>, gql::ChangeSetError> {
        let rows = sqlx::query_as::<_, ChangeSetRow>(sqlx::AssertSqlSafe::<&str>(&format!(
            "SELECT {CHANGE_SET_COLUMNS} FROM change_sets \
             WHERE ($1::text IS NULL OR kind=$1) AND ($2::text IS NULL OR status=$2) \
               AND ($3::text IS NULL OR scope_type=$3) AND ($4::text IS NULL OR scope_id=$4) \
             ORDER BY updated_at DESC,id DESC LIMIT $5"
        )))
        .bind(kind.map(gql::ChangeSetKind::as_str))
        .bind(status.map(gql::ChangeSetStatus::as_str))
        .bind(normalize_filter(scope_type))
        .bind(normalize_filter(scope_id))
        .bind(i64::from(limit.clamp(1, 500)))
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        load_change_sets_graphql(&self.pool, rows).await
    }

    async fn create_provider_price_change_set(
        &self,
        actor: i64,
        channel: ID,
        input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        use conduit_admin_graphql::change_package::ChangePackageServices as _;
        let base = self.provider_edit_base(channel.clone()).await?;
        let result = self
            .create_provider(
                actor,
                channel,
                ID(uuid::Uuid::new_v4().to_string()),
                base.revision,
                input,
                None,
            )
            .await?;
        let id = result
            .change_set_detail
            .ok_or_else(|| {
                gql::ChangeSetError::Invalid("NO_CHANGE: no package was created".into())
            })?
            .summary
            .id;
        load_change_set(&self.pool, parse_numeric_id(id.as_str(), "package")?).await
    }
    async fn create_retail_price_change_set(
        &self,
        actor: i64,
        book: ID,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        use conduit_admin_graphql::change_package::ChangePackageServices as _;
        let base = self.retail_edit_base(book.clone()).await?;
        let result = self
            .create_retail(
                actor,
                book,
                ID(uuid::Uuid::new_v4().to_string()),
                base.revision,
            )
            .await?;
        load_change_set(
            &self.pool,
            parse_numeric_id(result.summary.id.as_str(), "package")?,
        )
        .await
    }
    async fn save_retail_price_change_set_item(
        &self,
        actor: i64,
        mut input: gql::SaveRetailPriceChangeSetItemInput,
    ) -> Result<gql::ChangeSetItem, gql::ChangeSetError> {
        use conduit_admin_graphql::change_package::ChangePackageServices as _;
        // Internal callers supply a freshly read revision. Public mutations require their client's revision.
        if input.expected_revision.is_empty() {
            input.expected_revision =
                packages::current_revision(&self.pool, input.change_set_id.clone()).await?;
        }
        Ok(self.save_retail_item(actor, input).await?.item)
    }
    async fn submit_change_set(
        &self,
        actor: i64,
        id: ID,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        internal_transition(
            self,
            actor,
            id,
            conduit_admin_graphql::change_package::ChangeSetAvailableAction::Submit,
            None,
        )
        .await
    }
    async fn approve_change_set(
        &self,
        actor: i64,
        id: ID,
        note: Option<String>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        internal_transition(
            self,
            actor,
            id,
            conduit_admin_graphql::change_package::ChangeSetAvailableAction::Approve,
            note,
        )
        .await
    }
    async fn reject_change_set(
        &self,
        actor: i64,
        id: ID,
        note: Option<String>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        internal_transition(
            self,
            actor,
            id,
            conduit_admin_graphql::change_package::ChangeSetAvailableAction::Reject,
            note,
        )
        .await
    }
}
async fn internal_transition(
    adapter: &PgChangeSetAdapter,
    actor: i64,
    id: ID,
    action: conduit_admin_graphql::change_package::ChangeSetAvailableAction,
    note: Option<String>,
) -> Result<gql::ChangeSet, gql::ChangeSetError> {
    use conduit_admin_graphql::change_package::ChangePackageServices as _;
    let revision = packages::current_revision(&adapter.pool, id.clone()).await?;
    adapter
        .transition(actor, id.clone(), revision, action, note)
        .await?;
    load_change_set(&adapter.pool, parse_numeric_id(id.as_str(), "package")?).await
}

impl ChangeSetItemRow {
    fn into_graphql(self) -> Result<gql::ChangeSetItem, gql::ChangeSetError> {
        Ok(gql::ChangeSetItem {
            id: self.id.to_string().into(),
            item_key: self.item_key,
            action: parse_action(&self.action)?,
            before_snapshot: self.before_snapshot.map(|value| value.0.into()),
            after_snapshot: self.after_snapshot.map(|value| value.0.into()),
            source_snapshot: self.source_snapshot.map(|value| value.0.into()),
            validation_error: self.validation_error,
            created_at: conduit_admin_graphql::scalars::TimeScalar(self.created_at),
            updated_at: conduit_admin_graphql::scalars::TimeScalar(self.updated_at),
        })
    }
}

async fn load_change_set(pool: &PgPool, id: i64) -> Result<gql::ChangeSet, gql::ChangeSetError> {
    let row = sqlx::query_as::<_, ChangeSetRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_COLUMNS} FROM change_sets WHERE id=$1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(query_error)?
    .ok_or_else(|| gql::ChangeSetError::Invalid(format!("change set {id} not found")))?;
    load_change_sets_graphql(pool, vec![row])
        .await?
        .pop()
        .ok_or_else(|| gql::ChangeSetError::Invalid(format!("change set {id} not found")))
}

async fn load_change_sets_graphql(
    pool: &PgPool,
    rows: Vec<ChangeSetRow>,
) -> Result<Vec<gql::ChangeSet>, gql::ChangeSetError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let item_rows = sqlx::query_as::<_, ChangeSetItemRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items \
         WHERE change_set_id=ANY($1) ORDER BY change_set_id,id"
    )))
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(query_error)?;
    let event_rows = sqlx::query_as::<_, ChangeSetEventRow>(
        "SELECT id,change_set_id,event_type,actor_type,actor_id,detail,created_at \
         FROM change_set_events WHERE change_set_id=ANY($1) ORDER BY change_set_id,id",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(query_error)?;
    let mut items_by_change_set = std::collections::HashMap::<i64, Vec<_>>::new();
    for item in item_rows {
        let change_set_id = item.change_set_id;
        items_by_change_set
            .entry(change_set_id)
            .or_default()
            .push(item.into_graphql()?);
    }
    let mut events_by_change_set = std::collections::HashMap::<i64, Vec<_>>::new();
    for event in event_rows {
        events_by_change_set
            .entry(event.change_set_id)
            .or_default()
            .push(gql::ChangeSetEvent {
                id: event.id.to_string().into(),
                event_type: event.event_type,
                actor_type: event.actor_type,
                actor_id: event.actor_id.map(|id| id.to_string().into()),
                detail: event.detail.0.into(),
                created_at: conduit_admin_graphql::scalars::TimeScalar(event.created_at),
            });
    }
    rows.into_iter()
        .map(|row| {
            let id = row.id;
            change_set_row_into_graphql(
                row,
                items_by_change_set.remove(&id).unwrap_or_default(),
                events_by_change_set.remove(&id).unwrap_or_default(),
            )
        })
        .collect()
}

fn change_set_row_into_graphql(
    row: ChangeSetRow,
    items: Vec<gql::ChangeSetItem>,
    events: Vec<gql::ChangeSetEvent>,
) -> Result<gql::ChangeSet, gql::ChangeSetError> {
    Ok(gql::ChangeSet {
        id: row.id.to_string().into(),
        kind: parse_kind(&row.kind)?,
        scope_type: row.scope_type,
        scope_id: row.scope_id,
        title: row.title,
        status: parse_status(&row.status)?,
        base_revision: row.base_revision,
        source_revision: row.source_revision,
        applied_target_type: row.applied_target_type,
        applied_target_id: row.applied_target_id,
        validation_error: row.validation_error,
        created_by: row.created_by.map(|id| id.to_string().into()),
        submitted_by: row.submitted_by.map(|id| id.to_string().into()),
        reviewed_by: row.reviewed_by.map(|id| id.to_string().into()),
        review_note: row.review_note,
        created_at: conduit_admin_graphql::scalars::TimeScalar(row.created_at),
        updated_at: conduit_admin_graphql::scalars::TimeScalar(row.updated_at),
        submitted_at: row
            .submitted_at
            .map(conduit_admin_graphql::scalars::TimeScalar),
        reviewed_at: row
            .reviewed_at
            .map(conduit_admin_graphql::scalars::TimeScalar),
        applied_at: row
            .applied_at
            .map(conduit_admin_graphql::scalars::TimeScalar),
        items,
        events,
    })
}

async fn lock_change_set(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
) -> Result<ChangeSetRow, gql::ChangeSetError> {
    sqlx::query_as::<_, ChangeSetRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_COLUMNS} FROM change_sets WHERE id=$1 FOR UPDATE"
    )))
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage_error)?
    .ok_or_else(|| gql::ChangeSetError::Invalid(format!("change set {id} not found")))
}

async fn load_items_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    change_set_id: i64,
) -> Result<Vec<ChangeSetItemRow>, ApplyFailure> {
    sqlx::query_as::<_, ChangeSetItemRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items WHERE change_set_id=$1 ORDER BY id"
    )))
    .bind(change_set_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| ApplyFailure::Storage(error.to_string()))
}

async fn load_item_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    item_id: i64,
) -> Result<ChangeSetItemRow, gql::ChangeSetError> {
    sqlx::query_as::<_, ChangeSetItemRow>(sqlx::AssertSqlSafe::<&str>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items WHERE id=$1"
    )))
    .bind(item_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage_error)
}

async fn apply_provider_price(
    tx: &mut Transaction<'_, Postgres>,
    change_set: &ChangeSetRow,
    actor_user_id: i64,
) -> Result<(String, String), ApplyFailure> {
    let channel_id = parse_i64(&change_set.scope_id, "channel")?;
    crate::wiring_postgres_provider_pricing::lock_provider_pricing_channel(tx, channel_id)
        .await
        .map_err(ApplyFailure::Storage)?;
    crate::wiring::lock_accounting_currency_price_writes(tx)
        .await
        .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
    let settings = crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx)
        .await
        .map_err(|error| ApplyFailure::Invalid(error.to_string()))?;
    let (channel_type, channel_endpoints, channel_settings) = sqlx::query_as::<_, (String, SqlJson<Value>, SqlJson<Value>)>(
        "SELECT type,COALESCE(endpoints,'[]'::jsonb),settings FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_failure)?
    .ok_or_else(|| ApplyFailure::Invalid("channel not found".into()))?;
    let channel_settings = serde_json::from_value::<
        conduit_core::objects::channel_settings::ChannelSettings,
    >(channel_settings.0.clone())
    .map_err(|error| ApplyFailure::Invalid(format!("invalid channel settings: {error}")))?;
    let items = load_items_in_tx(tx, change_set.id).await?;
    if items.is_empty() {
        return Err(ApplyFailure::Invalid(
            "provider price change set is empty".into(),
        ));
    }
    let has_manual_source = items.iter().any(|item| {
        item.source_snapshot
            .as_ref()
            .and_then(|value| value.0.get("source"))
            .and_then(Value::as_str)
            == Some("manual")
    });
    let is_manual = items.iter().all(|item| {
        item.source_snapshot
            .as_ref()
            .and_then(|value| value.0.get("source"))
            .and_then(Value::as_str)
            == Some("manual")
    });
    if has_manual_source && !is_manual {
        return Err(ApplyFailure::Invalid(
            "provider price change set mixes manual and observed sources".into(),
        ));
    }
    let source_snapshot_id = if is_manual {
        None
    } else {
        let snapshot_id = parse_i64(&change_set.source_revision, "provider snapshot")?;
        let latest_snapshot_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM provider_price_snapshots WHERE channel_id=$1 AND status='success' \
             ORDER BY observed_at DESC,id DESC LIMIT 1",
        )
        .bind(channel_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_failure)?;
        if latest_snapshot_id != Some(snapshot_id) {
            return Err(ApplyFailure::Superseded(
                "a newer provider price observation exists".into(),
            ));
        }
        Some(snapshot_id)
    };
    for item in items {
        let source = item
            .source_snapshot
            .as_ref()
            .map(|value| &value.0)
            .ok_or_else(|| ApplyFailure::Invalid("provider price source is missing".into()))?;
        let source_currency = source
            .get("accountingCurrency")
            .and_then(Value::as_str)
            .ok_or_else(|| ApplyFailure::Invalid("provider price currency is missing".into()))?;
        if !source_currency.eq_ignore_ascii_case(&settings.currency) {
            return Err(ApplyFailure::Invalid(format!(
                "provider price currency {source_currency} does not match accounting currency {}",
                settings.currency
            )));
        }
        let source_version = source
            .get("accountingSettingsVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ApplyFailure::Invalid("provider price settings version is missing".into())
            })?;
        if source_version != settings.version {
            return Err(ApplyFailure::Superseded(format!(
                "provider price used accounting settings version {source_version}, current version is {}",
                settings.version
            )));
        }
        if !is_manual
            && (source["provenance"]["adoptionBlocked"] == true
                || (channel_type == "anthropic"
                    && channel_settings.management_adapter.as_deref() == Some("new_api")))
        {
            return Err(ApplyFailure::Invalid("NEWAPI_CLAUDE_RELAY_FACTS_UNPROVEN: observed native gateway prices cannot be adopted".into()));
        }
        let is_expression = item
            .after_snapshot
            .as_ref()
            .is_some_and(|value| value.0.get("kind").is_some());
        if !is_manual && !is_expression {
            crate::wiring_postgres_pricing_admission::validate_channel_billing_snapshot(
                source,
                &channel_settings.billing_currency,
                channel_settings.recharge_multiplier,
            )
            .map_err(ApplyFailure::Superseded)?;
        }
        let current =
            conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(tx, channel_id)
                .await
                .map_err(|error| ApplyFailure::Storage(error.to_string()))?
                .into_iter()
                .find(|row| row.model_id == item.item_key);
        let current_base = current.as_ref().map(base_price_snapshot);
        let expected_base = item.before_snapshot.as_ref().map(|value| value.0.clone());
        if current_base != expected_base {
            return Err(ApplyFailure::Superseded(format!(
                "formal procurement price for {} changed after staging",
                item.item_key
            )));
        }
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let before_snapshot = current
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
        let provider_row_id = source.get("providerPriceRowID").and_then(Value::as_i64);
        if item.action == "delete" {
            if item.after_snapshot.is_some() {
                return Err(ApplyFailure::Invalid(format!(
                    "deleted provider price {} must not have an after snapshot",
                    item.item_key
                )));
            }
            let existing = current.ok_or_else(|| {
                ApplyFailure::Superseded(format!(
                    "formal procurement price for {} no longer exists",
                    item.item_key
                ))
            })?;
            let head_id = parse_i64(&existing.id, "price head")?;
            conduit_db::PgChannelModelPriceRepo::archive_active_versions_in_tx(
                tx, head_id, &now_text,
            )
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
            conduit_db::PgChannelModelPriceRepo::soft_delete_price_in_tx(tx, head_id, &now_text)
                .await
                .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
            insert_provider_price_audit(
                tx,
                actor_user_id,
                change_set.id,
                channel_id,
                &item.item_key,
                before_snapshot,
                None,
                source_snapshot_id,
                provider_row_id,
                &settings.currency,
                settings.version,
            )
            .await?;
            continue;
        }
        if item.action != "create" && item.action != "update" {
            return Err(ApplyFailure::Invalid(format!(
                "unsupported provider price action {}",
                item.action
            )));
        }
        let proposed = item
            .after_snapshot
            .as_ref()
            .map(|value| &value.0)
            .ok_or_else(|| ApplyFailure::Invalid("provider price is missing".into()))?;
        let document =
            conduit_core::objects::retail_price_plan::RetailPriceDocument::parse(proposed)
                .map_err(ApplyFailure::Invalid)?;
        if proposed.get("schema").is_some() {
            return Err(ApplyFailure::Invalid(
                "provider price must be a single price leaf".into(),
            ));
        }
        if let Some(parsed) = document
            .global
            .as_ref()
            .and_then(|leaf| leaf.static_price())
        {
            crate::wiring::validate_model_price(parsed).map_err(ApplyFailure::Invalid)?;
        }
        let price_currency = if is_expression {
            if !crate::wiring_postgres_pricing_admission::expression_channel_supported(
                &channel_type,
                &channel_endpoints.0,
                &serde_json::to_value(&channel_settings).map_err(|_| {
                    ApplyFailure::Invalid("invalid channel billing settings".into())
                })?,
            ) {
                return Err(ApplyFailure::Invalid("EXPR_REQUIRES_UNPROVEN_USAGE_FACTS: procurement expressions require a native Anthropic channel".into()));
            }
            let accounting =
                crate::usage_charge_settler_postgres::load_accounting_settings_from(&mut **tx)
                    .await
                    .map_err(ApplyFailure::Invalid)?;
            accounting
                .real_to_accounting(rust_decimal::Decimal::ONE, "USD")
                .map_err(ApplyFailure::Invalid)?;
            if let Some(snapshot) = source_snapshot_id {
                let observed: Option<Value> = sqlx::query_scalar(
                    "SELECT provenance->'billingExpression' FROM provider_price_rows WHERE id=$1 AND snapshot_id=$2 AND channel_id=$3 AND upstream_model_id=$4 AND quality='exact'"
                ).bind(provider_row_id).bind(snapshot).bind(channel_id).bind(&item.item_key)
                 .fetch_optional(&mut **tx).await.map_err(db_failure)?;
                if observed.as_ref() != Some(proposed) {
                    return Err(ApplyFailure::Superseded(
                        "observed expression no longer matches the proposed leaf".into(),
                    ));
                }
            }
            "USD"
        } else {
            &settings.currency
        };
        let reference_id = crate::wiring::generate_reference_id();
        let applied = if let Some(existing) = current {
            let head_id = parse_i64(&existing.id, "price head")?;
            conduit_db::PgChannelModelPriceRepo::archive_active_versions_in_tx(
                tx, head_id, &now_text,
            )
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
            conduit_db::PgChannelModelPriceRepo::update_price_in_tx(
                tx,
                head_id,
                price_currency,
                proposed,
                &reference_id,
                &now_text,
            )
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?
        } else {
            conduit_db::PgChannelModelPriceRepo::create_price_in_tx(
                tx,
                channel_id,
                &item.item_key,
                price_currency,
                proposed,
                &reference_id,
                &now_text,
            )
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?
        };
        let applied_id = parse_i64(&applied.id, "applied price")?;
        conduit_db::PgChannelModelPriceRepo::create_version_in_tx(
            tx,
            channel_id,
            &item.item_key,
            applied_id,
            price_currency,
            proposed,
            VERSION_STATUS_ACTIVE,
            &now_text,
            &reference_id,
            &now_text,
        )
        .await
        .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
        insert_provider_price_audit(
            tx,
            actor_user_id,
            change_set.id,
            channel_id,
            &item.item_key,
            before_snapshot,
            serde_json::to_value(applied).ok(),
            source_snapshot_id,
            provider_row_id,
            &settings.currency,
            settings.version,
        )
        .await?;
    }
    sqlx::query("UPDATE channels SET updated_at=$2 WHERE id=$1 AND deleted_at=0")
        .bind(channel_id)
        .bind(Utc::now())
        .execute(&mut **tx)
        .await
        .map_err(db_failure)?;
    crate::wiring_postgres_price_formula::refresh_channel(tx, channel_id)
        .await
        .map_err(ApplyFailure::Storage)?;
    Ok(("channel".into(), channel_id.to_string()))
}

async fn apply_model_mapping(
    tx: &mut Transaction<'_, Postgres>,
    change_set: &ChangeSetRow,
) -> Result<(String, String), ApplyFailure> {
    let channel_id = parse_i64(&change_set.scope_id, "channel")?;
    if sqlx::query_scalar::<_, i64>(
        "SELECT id FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_failure)?
    .is_none()
    {
        return Err(ApplyFailure::Superseded(
            "mapping channel is no longer available".into(),
        ));
    }
    let items = load_items_in_tx(tx, change_set.id).await?;
    if items.is_empty() {
        return Err(ApplyFailure::Invalid(
            "model mapping package is empty".into(),
        ));
    }
    let mut metadata = std::collections::BTreeMap::<String, Value>::new();
    let mut prepared = Vec::new();
    for item in &items {
        let after = item.after_snapshot.as_ref().map(|v| &v.0).ok_or_else(|| {
            ApplyFailure::Invalid(format!("{}: mapping payload is missing", item.item_key))
        })?;
        let deployment = if let Some(upstream) = after["upstreamModelID"].as_str() {
            let available: bool = sqlx::query_scalar(
                "SELECT supported_models ? $2 FROM channels WHERE id=$1 AND deleted_at=0",
            )
            .bind(channel_id)
            .bind(upstream)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_failure)?;
            if !available {
                return Err(ApplyFailure::Superseded(
                    "model supply must be published before its mapping".into(),
                ));
            }
            sqlx::query_scalar::<_,i64>("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,variant,status,source) VALUES($1,$2,$2,'','enabled','discovered') ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET status='enabled',updated_at=now() RETURNING id").bind(channel_id).bind(upstream).fetch_one(&mut **tx).await.map_err(db_failure)?
        } else {
            after["deploymentID"]
                .as_i64()
                .ok_or_else(|| ApplyFailure::Invalid("mapping deployment is missing".into()))?
        };
        let public = after["publicModel"].as_object().ok_or_else(|| {
            ApplyFailure::Invalid(format!("{}: publicModel is missing", item.item_key))
        })?;
        let key = required_string(public, "modelID")?;
        for field in ["developer", "type", "name", "group"] {
            required_string(public, field)?;
        }
        if metadata
            .get(&key)
            .is_some_and(|old| old != &after["publicModel"])
        {
            return Err(ApplyFailure::Invalid(format!(
                "{}: mappings to {key} disagree about public model metadata",
                item.item_key
            )));
        }
        metadata.insert(key.clone(), after["publicModel"].clone());
        prepared.push((item, deployment, key));
    }
    // Lock all public keys before any insert. Fan-in routes compare against the
    // same pre-apply catalog, rather than a model just created by a prior item.
    for key in metadata.keys() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)")
            .bind(key)
            .execute(&mut **tx)
            .await
            .map_err(db_failure)?;
    }
    let mut model_ids = std::collections::BTreeMap::<String, i64>::new();
    for (item, deployment, key) in &prepared {
        let deployment_channel=sqlx::query_scalar::<_,i64>("SELECT channel_id FROM upstream_model_deployments WHERE id=$1 AND status='enabled' FOR UPDATE").bind(deployment).fetch_optional(&mut **tx).await.map_err(db_failure)?;
        if deployment_channel != Some(channel_id) {
            return Err(ApplyFailure::Superseded(format!(
                "{}: upstream deployment {deployment} is no longer available",
                item.item_key
            )));
        }
        let existing=sqlx::query("SELECT id,status,developer,\"type\",name,\"group\" FROM models WHERE model_id=$1 AND deleted_at=0 LIMIT 1 FOR UPDATE").bind(key).fetch_optional(&mut **tx).await.map_err(db_failure)?;
        let current = if let Some(row) = existing.as_ref() {
            let id: i64 = row.get("id");
            let route = sqlx::query_scalar::<_, String>(
                "SELECT status FROM model_routes WHERE public_model_id=$1 AND deployment_id=$2",
            )
            .bind(id)
            .bind(deployment)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_failure)?;
            Some(
                json!({"id":id,"status":row.get::<String,_>("status"),"developer":row.get::<String,_>("developer"),"type":row.get::<String,_>("type"),"name":row.get::<String,_>("name"),"group":row.get::<String,_>("group"),"routeStatus":route}),
            )
        } else {
            None
        };
        if current != item.before_snapshot.as_ref().map(|v| v.0.clone()) {
            return Err(ApplyFailure::Superseded(format!(
                "{}: public model or route for {key} changed after staging",
                item.item_key
            )));
        }
        if let Some(row) = existing {
            if row.get::<String, _>("status") == "archived" {
                return Err(ApplyFailure::Invalid(format!(
                    "{}: public model {key} is archived",
                    item.item_key
                )));
            }
            model_ids.insert(key.clone(), row.get("id"));
        }
    }
    for (key, public) in &metadata {
        if !model_ids.contains_key(key) {
            let id=sqlx::query_scalar::<_,i64>("INSERT INTO models(developer,model_id,\"type\",name,icon,\"group\",model_card,settings,status,remark,created_at,updated_at,deleted_at) VALUES($1,$2,$3,$4,'',$5,'{}'::jsonb,'{}'::jsonb,'enabled',$6,now(),now(),0) RETURNING id").bind(public["developer"].as_str()).bind(key).bind(public["type"].as_str()).bind(public["name"].as_str()).bind(public["group"].as_str()).bind(format!("Created from approved model mapping package {}",change_set.id)).fetch_one(&mut **tx).await.map_err(db_failure)?;
            model_ids.insert(key.clone(), id);
        }
    }
    let mut last = None;
    for (_, deployment, key) in &prepared {
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO model_routes(public_model_id,deployment_id,status,created_at,updated_at) VALUES($1,$2,'enabled',now(),now()) ON CONFLICT(public_model_id,deployment_id) DO UPDATE SET status='enabled',updated_at=now() RETURNING id").bind(model_ids[key]).bind(deployment).fetch_one(&mut **tx).await.map_err(db_failure)?;
        last = Some(id);
    }
    sqlx::query("UPDATE channels SET updated_at=now() WHERE id=$1 AND deleted_at=0")
        .bind(channel_id)
        .execute(&mut **tx)
        .await
        .map_err(db_failure)?;
    Ok(("model_route".into(), last.unwrap_or_default().to_string()))
}

async fn apply_retail_price(
    tx: &mut Transaction<'_, Postgres>,
    change_set: &ChangeSetRow,
    actor_user_id: i64,
) -> Result<(String, String), ApplyFailure> {
    let book_id = parse_i64(&change_set.scope_id, "price book")?;
    crate::wiring::lock_accounting_currency_price_writes(tx)
        .await
        .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
    let settings = crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(tx)
        .await
        .map_err(|error| ApplyFailure::Invalid(error.to_string()))?;
    if change_set.source_revision != settings.version.to_string() {
        return Err(ApplyFailure::Superseded(format!(
            "retail prices used accounting settings version {}, current version is {}",
            change_set.source_revision, settings.version
        )));
    }
    let currency =
        sqlx::query_scalar::<_, String>("SELECT currency FROM price_books WHERE id=$1 FOR UPDATE")
            .bind(book_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_failure)?
            .ok_or_else(|| ApplyFailure::Invalid(format!("price book {book_id} not found")))?;
    if !currency.eq_ignore_ascii_case(&settings.currency) {
        return Err(ApplyFailure::Invalid(format!(
            "price book currency {currency} does not match accounting currency {}",
            settings.currency
        )));
    }
    let current_reference = sqlx::query_scalar::<_, String>(
        "SELECT reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' \
         ORDER BY version DESC LIMIT 1",
    )
    .bind(book_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_failure)?
    .unwrap_or_default();
    if current_reference != change_set.base_revision {
        return Err(ApplyFailure::Superseded(
            "the published retail price version changed after this change set was created".into(),
        ));
    }
    let items = load_items_in_tx(tx, change_set.id).await?;
    if items.is_empty() {
        return Err(ApplyFailure::Invalid(
            "cannot apply an empty retail price change set".into(),
        ));
    }
    let mut validated = Vec::with_capacity(items.len());
    for item in items {
        let public_model_id = parse_i64(&item.item_key, "public model")?;
        if let (Some(source), Some(proposed)) = (&item.source_snapshot, &item.after_snapshot) {
            crate::wiring_postgres_price_formula::validate_derived(
                tx,
                book_id,
                public_model_id,
                &source.0,
                &proposed.0,
            )
            .await
            .map_err(ApplyFailure::Superseded)?;
        }
        let price = item
            .after_snapshot
            .map(|value| value.0)
            .ok_or_else(|| ApplyFailure::Invalid("retail price is missing".into()))?;
        let document = conduit_core::objects::retail_price_plan::RetailPriceDocument::parse(&price)
            .map_err(ApplyFailure::Invalid)?;
        if document
            .leaves()
            .iter()
            .any(|leaf| leaf.static_price().is_none())
        {
            crate::wiring_postgres_pricing_admission::expression_retail_route_warnings(
                tx,
                public_model_id,
            )
            .await
            .map_err(ApplyFailure::Invalid)?;
            let accounting =
                crate::usage_charge_settler_postgres::load_accounting_settings_from(&mut **tx)
                    .await
                    .map_err(ApplyFailure::Invalid)?;
            accounting
                .real_to_accounting(rust_decimal::Decimal::ONE, "USD")
                .map_err(ApplyFailure::Invalid)?;
        }
        for leaf in document
            .leaves()
            .into_iter()
            .filter_map(|leaf| leaf.static_price())
        {
            crate::wiring::validate_model_price(leaf).map_err(ApplyFailure::Invalid)?;
        }
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM models WHERE id=$1 AND deleted_at=0)",
        )
        .bind(public_model_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_failure)?;
        if !exists {
            return Err(ApplyFailure::Invalid(format!(
                "public model {public_model_id} not found"
            )));
        }
        validated.push((public_model_id, price));
    }
    let base_items=sqlx::query_as::<_,(i64,SqlJson<Value>)>("SELECT i.public_model_id,i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.reference_id=$2").bind(book_id).bind(&current_reference).fetch_all(&mut **tx).await.map_err(db_failure)?;
    let mut full = base_items
        .into_iter()
        .map(|(id, price)| (id, price.0))
        .collect::<std::collections::BTreeMap<_, _>>();
    for (id, price) in validated {
        full.insert(id, price);
    }
    let validated = full.into_iter().collect::<Vec<_>>();
    let before_snapshot =
        super::wiring_postgres_commercialization::price_book_state_snapshot(tx, book_id)
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
    let version = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(version),0)+1 FROM price_book_versions WHERE price_book_id=$1",
    )
    .bind(book_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_failure)?;
    let now = Utc::now();
    sqlx::query(
        "UPDATE price_book_versions SET status='archived',effective_end_at=$1,updated_at=$1 \
         WHERE price_book_id=$2 AND status='published'",
    )
    .bind(now)
    .bind(book_id)
    .execute(&mut **tx)
    .await
    .map_err(db_failure)?;
    let reference_id = format!("price-book-{book_id}-v{version}-{}", now.timestamp_micros());
    let version_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO price_book_versions \
         (price_book_id,version,status,reference_id,effective_start_at,created_at,updated_at) \
         VALUES($1,$2,'published',$3,$4,$4,$4) RETURNING id",
    )
    .bind(book_id)
    .bind(version)
    .bind(reference_id)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_failure)?;
    for (public_model_id, price) in validated {
        sqlx::query(
            "INSERT INTO price_book_items \
             (price_book_version_id,public_model_id,price,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$4)",
        )
        .bind(version_id)
        .bind(public_model_id)
        .bind(SqlJson(price))
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(db_failure)?;
    }
    let after_snapshot =
        super::wiring_postgres_commercialization::price_book_state_snapshot(tx, book_id)
            .await
            .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
    super::wiring_postgres_commercialization::insert_pricing_audit(
        tx,
        Some(actor_user_id),
        "apply_retail_price_change_set",
        "price_book_version",
        &version_id.to_string(),
        Some(before_snapshot),
        Some(after_snapshot),
        &settings,
    )
    .await
    .map_err(|error| ApplyFailure::Storage(error.to_string()))?;
    // The currently applying formula draft must become terminal before the
    // new base is reconciled, so automatic refresh cannot supersede itself.
    sqlx::query("UPDATE change_sets SET formula_id=NULL WHERE id=$1")
        .bind(change_set.id)
        .execute(&mut **tx)
        .await
        .map_err(db_failure)?;
    crate::wiring_postgres_price_formula::refresh_book(tx, book_id)
        .await
        .map_err(ApplyFailure::Storage)?;
    Ok(("price_book_version".into(), version_id.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn insert_provider_price_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_user_id: i64,
    change_set_id: i64,
    channel_id: i64,
    model_id: &str,
    before_snapshot: Option<Value>,
    after_snapshot: Option<Value>,
    source_snapshot_id: Option<i64>,
    source_observation_id: Option<i64>,
    currency: &str,
    settings_version: u64,
) -> Result<(), ApplyFailure> {
    sqlx::query(
        "INSERT INTO pricing_change_audits \
         (actor_type,actor_id,operation,entity_type,entity_id,before_snapshot,after_snapshot,\
          source_snapshot_id,source_observation_id,source_change_set_id,accounting_currency,\
          accounting_settings_version,result,request_correlation_id,created_at) \
         VALUES('user',$1,'apply_provider_price_change_set','channel_model_price',$2,$3,$4,\
                $5,$6,$7,$8,$9,'success',$10,$11)",
    )
    .bind(actor_user_id)
    .bind(format!("{channel_id}:{model_id}"))
    .bind(before_snapshot)
    .bind(after_snapshot)
    .bind(source_snapshot_id)
    .bind(source_observation_id)
    .bind(change_set_id)
    .bind(currency)
    .bind(i64::try_from(settings_version).unwrap_or(i64::MAX))
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(Utc::now())
    .execute(&mut **tx)
    .await
    .map_err(db_failure)?;
    Ok(())
}

async fn mark_reviewed(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    status: &str,
    actor_user_id: i64,
    review_note: Option<&str>,
    target: Option<(&str, &str)>,
    now: DateTime<Utc>,
) -> Result<(), gql::ChangeSetError> {
    let (target_type, target_id) = target
        .map(|(kind, id)| (Some(kind), Some(id)))
        .unwrap_or((None, None));
    sqlx::query(
        "UPDATE change_sets SET status=$2,validation_error=NULL,reviewed_by=$3,review_note=$4,\
         reviewed_at=$5,applied_at=CASE WHEN $2='applied' THEN $5 ELSE NULL END,\
         applied_target_type=$6,applied_target_id=$7,updated_at=$5 WHERE id=$1",
    )
    .bind(id)
    .bind(status)
    .bind(actor_user_id)
    .bind(review_note)
    .bind(now)
    .bind(target_type)
    .bind(target_id)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn mark_invalid(
    tx: &mut Transaction<'_, Postgres>,
    id: i64,
    actor_user_id: i64,
    review_note: Option<&str>,
    validation_error: &str,
    now: DateTime<Utc>,
) -> Result<(), gql::ChangeSetError> {
    sqlx::query(
        "UPDATE change_sets SET status='invalid',validation_error=$2,reviewed_by=$3,review_note=$4,\
         reviewed_at=$5,applied_at=NULL,applied_target_type=NULL,applied_target_id=NULL,updated_at=$5 \
         WHERE id=$1",
    )
    .bind(id)
    .bind(validation_error)
    .bind(actor_user_id)
    .bind(review_note)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

pub(crate) async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    change_set_id: i64,
    event_type: &str,
    actor_type: &str,
    actor_id: Option<i64>,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<(), gql::ChangeSetError> {
    sqlx::query(
        "INSERT INTO change_set_events \
         (change_set_id,event_type,actor_type,actor_id,detail,created_at) VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(change_set_id)
    .bind(event_type)
    .bind(actor_type)
    .bind(actor_id)
    .bind(SqlJson(detail))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn base_price_snapshot(row: &conduit_db::row::ChannelModelPriceRow) -> Value {
    json!({
        "id": row.id.parse::<i64>().ok(),
        "currencyCode": row.currency_code,
        "price": row.price,
        "referenceID": row.reference_id,
    })
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, ApplyFailure> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApplyFailure::Invalid(format!("{key} is required")))
}

fn parse_kind(value: &str) -> Result<gql::ChangeSetKind, gql::ChangeSetError> {
    match value {
        "provider_price" => Ok(gql::ChangeSetKind::ProviderPrice),
        "model_mapping" => Ok(gql::ChangeSetKind::ModelMapping),
        "retail_price" => Ok(gql::ChangeSetKind::RetailPrice),
        "channel_configuration" => Ok(gql::ChangeSetKind::ChannelConfiguration),
        "model_supply" => Ok(gql::ChangeSetKind::ModelSupply),
        "group_access" => Ok(gql::ChangeSetKind::GroupAccess),
        "identity_access" => Ok(gql::ChangeSetKind::IdentityAccess),
        "project_configuration" => Ok(gql::ChangeSetKind::ProjectConfiguration),
        "system_configuration" => Ok(gql::ChangeSetKind::SystemConfiguration),
        "project_credit" => Ok(gql::ChangeSetKind::ProjectCredit),
        other => Err(gql::ChangeSetError::Query(format!(
            "unknown change set kind {other}"
        ))),
    }
}

fn parse_status(value: &str) -> Result<gql::ChangeSetStatus, gql::ChangeSetError> {
    match value {
        "draft" => Ok(gql::ChangeSetStatus::Draft),
        "pending_review" => Ok(gql::ChangeSetStatus::PendingReview),
        "approved" => Ok(gql::ChangeSetStatus::Approved),
        "applied" => Ok(gql::ChangeSetStatus::Applied),
        "rejected" => Ok(gql::ChangeSetStatus::Rejected),
        "superseded" => Ok(gql::ChangeSetStatus::Superseded),
        "invalid" => Ok(gql::ChangeSetStatus::Invalid),
        other => Err(gql::ChangeSetError::Query(format!(
            "unknown change set status {other}"
        ))),
    }
}

fn parse_action(value: &str) -> Result<gql::ChangeSetAction, gql::ChangeSetError> {
    match value {
        "create" => Ok(gql::ChangeSetAction::Create),
        "update" => Ok(gql::ChangeSetAction::Update),
        "delete" => Ok(gql::ChangeSetAction::Delete),
        other => Err(gql::ChangeSetError::Query(format!(
            "unknown change set action {other}"
        ))),
    }
}

fn parse_numeric_id(value: &str, label: &str) -> Result<i64, gql::ChangeSetError> {
    value
        .rsplit('/')
        .next()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| gql::ChangeSetError::Invalid(format!("invalid {label} id: {value}")))
}

fn parse_i64(value: &str, label: &str) -> Result<i64, ApplyFailure> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| ApplyFailure::Invalid(format!("invalid {label} id: {value}")))
}

fn normalize_note(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.chars().take(2_000).collect())
    })
}

fn normalize_filter(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn query_error(error: sqlx::Error) -> gql::ChangeSetError {
    gql::ChangeSetError::Query(error.to_string())
}

fn storage_error(error: impl std::fmt::Display) -> gql::ChangeSetError {
    gql::ChangeSetError::Storage(error.to_string())
}

fn db_failure(error: sqlx::Error) -> ApplyFailure {
    ApplyFailure::Storage(error.to_string())
}

#[derive(Debug)]
enum ApplyFailure {
    Superseded(String),
    Invalid(String),
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::change_set::ChangeSetServices as _;
    use conduit_core::objects::pricing::{
        ModelPriceItem, PRICING_MODE_USAGE_PER_UNIT, Pricing, price_item_code,
    };
    use rust_decimal::Decimal;

    async fn publish_reviewed(
        pool: &PgPool,
        actor: i64,
        reviewed: gql::ChangeSet,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        use conduit_admin_graphql::change_package::ChangePackageServices;
        let adapter = PgChangeSetAdapter::new(pool.clone());
        let detail = super::packages::required_detail(
            pool,
            actor,
            reviewed
                .id
                .as_str()
                .parse()
                .map_err(|_| gql::ChangeSetError::Invalid("invalid test id".into()))?,
        )
        .await?;
        adapter
            .transition(
                actor,
                reviewed.id.clone(),
                detail.summary.revision,
                conduit_admin_graphql::change_package::ChangeSetAvailableAction::Publish,
                None,
            )
            .await?;
        load_change_set(
            pool,
            reviewed
                .id
                .as_str()
                .parse()
                .map_err(|_| gql::ChangeSetError::Invalid("invalid test id".into()))?,
        )
        .await
    }

    fn token_price() -> ModelPrice {
        ModelPrice {
            items: [
                price_item_code::USAGE,
                price_item_code::COMPLETION,
                price_item_code::PROMPT_CACHED_TOKEN,
                price_item_code::WRITE_CACHED_TOKENS,
            ]
            .into_iter()
            .map(|code| ModelPriceItem {
                item_code: code.into(),
                pricing: Pricing {
                    mode: PRICING_MODE_USAGE_PER_UNIT.into(),
                    usage_per_unit: Some(Decimal::ONE),
                    ..Pricing::default()
                },
                ..ModelPriceItem::default()
            })
            .collect(),
        }
    }

    fn token_price_input_for(
        model_id: &str,
    ) -> conduit_admin_graphql::model_ext::SaveChannelModelPriceInput {
        conduit_admin_graphql::model_ext::SaveChannelModelPriceInput {
            model_id: model_id.into(),
            currency_code: "CNY".into(),
            billing_expression: None,
            price: Some(conduit_admin_graphql::model_ext::ModelPriceInput {
                items: [
                    conduit_admin_graphql::request_usage::PriceItemCode::PromptTokens,
                    conduit_admin_graphql::request_usage::PriceItemCode::CompletionTokens,
                    conduit_admin_graphql::request_usage::PriceItemCode::PromptCachedTokens,
                    conduit_admin_graphql::request_usage::PriceItemCode::PromptWriteCachedTokens,
                ]
                .into_iter()
                .map(
                    |item_code| conduit_admin_graphql::model_ext::ModelPriceItemInput {
                        item_code,
                        pricing: conduit_admin_graphql::model_ext::PricingInput {
                            mode: conduit_admin_graphql::model_ext::PricingMode::UsagePerUnit,
                            flat_fee: None,
                            usage_per_unit: Some(conduit_admin_graphql::scalars::DecimalScalar(
                                Decimal::ONE,
                            )),
                            usage_tiered: None,
                        },
                        prompt_write_cache_variants: None,
                    },
                )
                .collect(),
            }),
        }
    }

    fn token_price_input() -> conduit_admin_graphql::model_ext::SaveChannelModelPriceInput {
        token_price_input_for("model-b")
    }

    #[tokio::test]
    async fn change_sets_are_ordered_by_latest_activity_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        database.owners(&[7, 8]).await?;
        let older_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,created_at,updated_at) \
             VALUES('retail_price','price_book','1','older draft','draft',now() - interval '2 days',now()) \
             RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let newer_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,created_at,updated_at) \
             VALUES('retail_price','price_book','2','newer draft','draft',now() - interval '1 day',now() - interval '1 hour') \
             RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;

        let adapter = PgChangeSetAdapter::new(database.pool.clone());
        let rows = adapter
            .change_sets(Some(gql::ChangeSetKind::RetailPrice), None, None, None, 10)
            .await?;

        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec![older_id.to_string(), newer_id.to_string()]
        );

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn manual_provider_price_draft_applies_creates_and_deletes_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        database.owners(&[7, 8]).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai','manual-price-change-set','enabled','{}'::jsonb,\
                    '[\"model-a\",\"model-b\"]'::jsonb,'model-a',\
                    '{\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}'::jsonb) RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let old_price = serde_json::to_value(token_price())?;
        let old_head_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_model_prices \
             (channel_id,model_id,currency_code,price,reference_id) \
             VALUES($1,'model-a','CNY',$2,'manual-old-head') RETURNING id",
        )
        .bind(channel_id)
        .bind(SqlJson(old_price.clone()))
        .fetch_one(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO channel_model_price_versions \
             (channel_id,model_id,channel_model_price_id,currency_code,price,status,\
              effective_start_at,reference_id) \
             VALUES($1,'model-a',$2,'CNY',$3,'active',now(),'manual-old-version')",
        )
        .bind(channel_id)
        .bind(old_head_id)
        .bind(SqlJson(old_price))
        .execute(&database.pool)
        .await?;

        let adapter = PgChangeSetAdapter::new(database.pool.clone());
        let draft = adapter
            .create_provider_price_change_set(
                7,
                channel_id.to_string().into(),
                vec![token_price_input()],
            )
            .await?;
        assert_eq!(draft.status, gql::ChangeSetStatus::Draft);
        assert_eq!(draft.items.len(), 2);
        assert!(draft.items.iter().any(|item| {
            item.item_key == "model-a" && item.action == gql::ChangeSetAction::Delete
        }));
        assert!(draft.items.iter().any(|item| {
            item.item_key == "model-b" && item.action == gql::ChangeSetAction::Create
        }));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0"
            )
            .bind(channel_id)
            .fetch_one(&database.pool)
            .await?,
            1,
            "staging must not change formal prices"
        );

        let observed_change_set_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,source_revision,submitted_at,created_at,updated_at) \
             VALUES('provider_price','channel',$1,'observed price','pending_review','99',now(),now(),now()) \
             RETURNING id",
        )
        .bind(channel_id.to_string())
        .fetch_one(&database.pool)
        .await?;
        let proposed_model_b = draft
            .items
            .iter()
            .find(|item| item.item_key == "model-b")
            .and_then(|item| item.after_snapshot.as_ref())
            .map(|value| value.0.clone())
            .expect("model-b proposal");
        sqlx::query(
            "INSERT INTO change_set_items \
             (change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES($1,'model-b','create',$2,$3,now(),now())",
        )
        .bind(observed_change_set_id)
        .bind(SqlJson(proposed_model_b))
        .bind(SqlJson(json!({
            "accountingCurrency": "CNY",
            "accountingSettingsVersion": 1,
        })))
        .execute(&database.pool)
        .await?;

        let submitted = adapter.submit_change_set(7, draft.id.clone()).await?;
        assert_eq!(submitted.status, gql::ChangeSetStatus::PendingReview);
        assert_eq!(
            sqlx::query_as::<_, (String, Option<i64>)>(
                "SELECT status,reviewed_by FROM change_sets WHERE id=$1"
            )
            .bind(observed_change_set_id)
            .fetch_one(&database.pool)
            .await?,
            ("pending_review".into(), None),
            "a new manual intent must not silently supersede another observation"
        );
        let applied = adapter
            .approve_change_set(8, draft.id, Some("manual prices verified".into()))
            .await?;
        assert_eq!(applied.status, gql::ChangeSetStatus::Approved);
        let applied = publish_reviewed(&database.pool, 8, applied).await?;
        assert_eq!(applied.status, gql::ChangeSetStatus::Applied);
        let formal_models = sqlx::query_scalar::<_, String>(
            "SELECT model_id FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0",
        )
        .bind(channel_id)
        .fetch_all(&database.pool)
        .await?;
        assert_eq!(formal_models, vec!["model-b"]);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pricing_change_audits WHERE source_change_set_id=$1 \
                 AND source_snapshot_id IS NULL"
            )
            .bind(applied.id.as_str().parse::<i64>()?)
            .fetch_one(&database.pool)
            .await?,
            2
        );

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_approval_rolls_back_partial_writes_and_persists_failure_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        database.owners(&[7, 8]).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai','invalid-price-change-set','enabled','{}'::jsonb,\
                    '[\"model-a\",\"model-b\"]'::jsonb,'model-a',\
                    '{\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}'::jsonb) RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let adapter = PgChangeSetAdapter::new(database.pool.clone());
        let draft = adapter
            .create_provider_price_change_set(
                7,
                channel_id.to_string().into(),
                vec![
                    token_price_input_for("model-a"),
                    token_price_input_for("model-b"),
                ],
            )
            .await?;
        let change_set_id = draft.id.as_str().parse::<i64>()?;
        adapter.submit_change_set(7, draft.id.clone()).await?;
        sqlx::query(
            "UPDATE change_set_items SET after_snapshot=$3 \
             WHERE change_set_id=$1 AND item_key=$2",
        )
        .bind(change_set_id)
        .bind("model-b")
        .bind(SqlJson(json!({
            "items": [{
                "itemCode": "prompt_tokens",
                "pricing": {"mode": "usage_per_unit"}
            }]
        })))
        .execute(&database.pool)
        .await?;

        let approval = adapter
            .approve_change_set(8, draft.id, Some("reviewed".into()))
            .await;
        assert!(matches!(approval, Err(gql::ChangeSetError::Invalid(_))));
        let state = sqlx::query_as::<_, (String, Option<String>, Option<i64>)>(
            "SELECT status,validation_error,reviewed_by FROM change_sets WHERE id=$1",
        )
        .bind(change_set_id)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(state.0, "invalid");
        assert!(
            state
                .1
                .as_deref()
                .is_some_and(|error| error.contains("one or more package items are invalid"))
        );
        let item_error = sqlx::query_scalar::<_, String>("SELECT validation_error FROM change_set_items WHERE change_set_id=$1 AND item_key='model-b'")
            .bind(change_set_id).fetch_one(&database.pool).await?;
        assert!(item_error.contains("usagePerUnit is required"));
        assert_eq!(state.2, Some(8));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0"
            )
            .bind(channel_id)
            .fetch_one(&database.pool)
            .await?,
            0,
            "the savepoint must roll back prices written before validation failed"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM change_set_events WHERE change_set_id=$1 AND event_type='validation_failed'"
            )
            .bind(change_set_id)
            .fetch_one(&database.pool)
            .await?,
            1
        );

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn provider_price_approval_applies_all_prompt_cache_prices_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        database.owners(&[7, 8]).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai','change-set-provider','enabled','{}'::jsonb,\
                    '[\"model-a\"]'::jsonb,'model-a',\
                    '{\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}'::jsonb) RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let snapshot_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO provider_price_snapshots \
             (channel_id,adapter_id,adapter_version,attempted_endpoints,status,warnings,started_at,observed_at) \
             VALUES($1,'test','1','[]'::jsonb,'success','[]'::jsonb,now(),now()) RETURNING id",
        )
        .bind(channel_id)
        .fetch_one(&database.pool)
        .await?;
        let provider_row_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO provider_price_rows \
             (snapshot_id,channel_id,upstream_model_id,group_name,billing_kind,quality) \
             VALUES($1,$2,'model-a','','tokens','verified') RETURNING id",
        )
        .bind(snapshot_id)
        .bind(channel_id)
        .fetch_one(&database.pool)
        .await?;
        let price = serde_json::to_value(token_price())?;
        let now = Utc::now();
        let change_set_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,source_revision,submitted_at,created_at,updated_at) \
             VALUES('provider_price','channel',$1,'provider price','pending_review',$2,$3,$3,$3) \
             RETURNING id",
        )
        .bind(channel_id.to_string())
        .bind(snapshot_id.to_string())
        .bind(now)
        .fetch_one(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO change_set_items \
             (change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES($1,'model-a','create',$2,$3,$4,$4)",
        )
        .bind(change_set_id)
        .bind(SqlJson(price.clone()))
        .bind(SqlJson(json!({
            "accountingCurrency": "CNY",
            "accountingSettingsVersion": 1,
            "billingCurrency": "CNY",
            "rechargeMultiplier": "1",
            "providerPriceRowID": provider_row_id,
            "snapshotID": snapshot_id,
        })))
        .bind(now)
        .execute(&database.pool)
        .await?;

        let adapter = PgChangeSetAdapter::new(database.pool.clone());
        let applied = adapter
            .approve_change_set(7, change_set_id.to_string().into(), Some("verified".into()))
            .await?;
        assert_eq!(applied.status, gql::ChangeSetStatus::Approved);
        let applied = publish_reviewed(&database.pool, 7, applied).await?;
        assert_eq!(applied.status, gql::ChangeSetStatus::Applied);
        assert_eq!(applied.applied_target_type.as_deref(), Some("channel"));
        let stored = sqlx::query_as::<_, (String, SqlJson<Value>)>(
            "SELECT currency_code,price FROM channel_model_prices \
             WHERE channel_id=$1 AND model_id='model-a' AND deleted_at=0",
        )
        .bind(channel_id)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(stored.0, "CNY");
        assert_eq!(stored.1.0, price);
        let event_mutation = sqlx::query(
            "UPDATE change_set_events SET event_type='tampered' WHERE change_set_id=$1",
        )
        .bind(change_set_id)
        .execute(&database.pool)
        .await;
        assert!(
            event_mutation.is_err(),
            "change-set events must be append-only"
        );

        database.cleanup().await?;
        Ok(())
    }
}

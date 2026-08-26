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

    let change_set_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO change_sets \
         (kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) \
         VALUES('provider_price','channel',$1,$2,'draft','',$3,$4,$5,$5) RETURNING id",
    )
    .bind(target_channel_id.to_string())
    .bind(format!("Duplicated procurement prices: {target_channel_name}"))
    .bind(accounting_settings_version.to_string())
    .bind(actor_user_id)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage_error)?;
    let source = json!({
        "source": "manual",
        "origin": "channel_duplicate",
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
        let rows = sqlx::query_as::<_, ChangeSetRow>(&format!(
            "SELECT {CHANGE_SET_COLUMNS} FROM change_sets \
             WHERE ($1::text IS NULL OR kind=$1) AND ($2::text IS NULL OR status=$2) \
               AND ($3::text IS NULL OR scope_type=$3) AND ($4::text IS NULL OR scope_id=$4) \
             ORDER BY updated_at DESC,id DESC LIMIT $5"
        ))
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
        actor_user_id: i64,
        channel_id: ID,
        input: Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        let channel_id = parse_numeric_id(channel_id.as_str(), "channel")?;
        let mut seen = std::collections::HashSet::new();
        let mut prepared = Vec::with_capacity(input.len());
        for item in input {
            if !seen.insert(item.model_id.clone()) {
                return Err(gql::ChangeSetError::Invalid(format!(
                    "duplicate model price input: model_id={}",
                    item.model_id
                )));
            }
            let currency = crate::wiring::normalize_price_currency_code(&item.currency_code)
                .map_err(gql::ChangeSetError::Invalid)?;
            let price = crate::conv::model_price_input_to_core(item.price);
            crate::wiring::validate_model_price(&price).map_err(|error| {
                gql::ChangeSetError::Invalid(format!(
                    "invalid model price for {}: {error}",
                    item.model_id
                ))
            })?;
            if price.items.is_empty() {
                return Err(gql::ChangeSetError::Invalid(format!(
                    "provider price for {} must contain at least one item",
                    item.model_id
                )));
            }
            let price_json = serde_json::to_value(&price).map_err(|error| {
                gql::ChangeSetError::Invalid(format!(
                    "cannot encode provider price for {}: {error}",
                    item.model_id
                ))
            })?;
            prepared.push((item.model_id, currency, price, price_json));
        }

        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        crate::wiring_postgres_provider_pricing::lock_provider_pricing_channel(&mut tx, channel_id)
            .await
            .map_err(gql::ChangeSetError::Storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage_error)?;
        let settings = crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(&mut tx)
            .await
            .map_err(|error| gql::ChangeSetError::Invalid(error.to_string()))?;
        if let Some((model_id, currency, _, _)) = prepared
            .iter()
            .find(|(_, currency, _, _)| !currency.eq_ignore_ascii_case(&settings.currency))
        {
            return Err(gql::ChangeSetError::Invalid(format!(
                "procurement price for model {model_id} must use accounting currency {}, got {currency}",
                settings.currency
            )));
        }
        let channel_name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(channel_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| gql::ChangeSetError::Invalid(format!("channel {channel_id} not found")))?;
        let existing =
            conduit_db::PgChannelModelPriceRepo::list_prices_by_channel_in_tx(&mut tx, channel_id)
                .await
                .map_err(storage_error)?;
        let actions = crate::wiring::calculate_price_changes(&existing, &prepared);
        if actions
            .iter()
            .all(|action| matches!(action, crate::wiring::PriceAction::Skip))
        {
            return Err(gql::ChangeSetError::Invalid(
                "the submitted procurement prices do not change the formal price list".into(),
            ));
        }

        let existing_draft = sqlx::query_scalar::<_, i64>(
            "SELECT cs.id FROM change_sets cs WHERE cs.kind='provider_price' \
             AND cs.scope_type='channel' AND cs.scope_id=$1 AND cs.status='draft' \
             AND EXISTS(SELECT 1 FROM change_set_items item WHERE item.change_set_id=cs.id \
                        AND item.source_snapshot->>'source'='manual') \
             ORDER BY cs.id DESC LIMIT 1",
        )
        .bind(channel_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?;
        let now = Utc::now();
        let title = format!("Manual procurement prices: {channel_name}");
        let change_set_id = if let Some(id) = existing_draft {
            sqlx::query(
                "UPDATE change_sets SET title=$2,base_revision='',source_revision=$3,\
                 validation_error=NULL,updated_at=$4 WHERE id=$1",
            )
            .bind(id)
            .bind(&title)
            .bind(settings.version.to_string())
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
            sqlx::query("DELETE FROM change_set_items WHERE change_set_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(storage_error)?;
            id
        } else {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO change_sets \
                 (kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) \
                 VALUES('provider_price','channel',$1,$2,'draft','',$3,$4,$5,$5) RETURNING id",
            )
            .bind(channel_id.to_string())
            .bind(&title)
            .bind(settings.version.to_string())
            .bind(actor_user_id)
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage_error)?
        };
        let source = json!({
            "source": "manual",
            "accountingCurrency": settings.currency,
            "accountingSettingsVersion": settings.version,
        });
        let mut changed_items = 0_u64;
        for action in actions {
            let (item_key, action, before, after) = match action {
                crate::wiring::PriceAction::Skip => continue,
                crate::wiring::PriceAction::Delete { existing } => (
                    existing.model_id.clone(),
                    "delete",
                    Some(base_price_snapshot(&existing)),
                    None,
                ),
                crate::wiring::PriceAction::Create {
                    model_id,
                    price_json,
                    ..
                } => (model_id, "create", None, Some(price_json)),
                crate::wiring::PriceAction::Update {
                    existing,
                    model_id,
                    price_json,
                    ..
                } => (
                    model_id,
                    "update",
                    Some(base_price_snapshot(&existing)),
                    Some(price_json),
                ),
            };
            sqlx::query(
                "INSERT INTO change_set_items \
                 (change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,$5,$6,$7,$7)",
            )
            .bind(change_set_id)
            .bind(item_key)
            .bind(action)
            .bind(before.map(SqlJson))
            .bind(after.map(SqlJson))
            .bind(SqlJson(source.clone()))
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
            changed_items += 1;
        }
        insert_event(
            &mut tx,
            change_set_id,
            if existing_draft.is_some() {
                "items_replaced"
            } else {
                "created"
            },
            "user",
            Some(actor_user_id),
            json!({"source": "manual", "itemCount": changed_items}),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        load_change_set(&self.pool, change_set_id).await
    }

    async fn create_retail_price_change_set(
        &self,
        actor_user_id: i64,
        price_book_id: ID,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        let book_id = parse_numeric_id(price_book_id.as_str(), "price book")?;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage_error)?;
        let settings = crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(&mut tx)
            .await
            .map_err(|error| gql::ChangeSetError::Invalid(error.to_string()))?;
        let book = sqlx::query("SELECT name,currency FROM price_books WHERE id=$1 FOR UPDATE")
            .bind(book_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage_error)?
            .ok_or_else(|| {
                gql::ChangeSetError::Invalid(format!("price book {book_id} not found"))
            })?;
        let book_name: String = book.get("name");
        let currency: String = book.get("currency");
        if !currency.eq_ignore_ascii_case(&settings.currency) {
            return Err(gql::ChangeSetError::Invalid(format!(
                "price book currency {currency} does not match accounting currency {}",
                settings.currency
            )));
        }
        let pending_review = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM change_sets WHERE kind='retail_price' AND scope_type='price_book' \
             AND scope_id=$1 AND status='pending_review' ORDER BY id DESC LIMIT 1",
        )
        .bind(book_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?;
        if let Some(id) = pending_review {
            return Err(gql::ChangeSetError::Invalid(format!(
                "retail price change set {id} is awaiting review"
            )));
        }
        if let Some(existing) = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM change_sets WHERE kind='retail_price' AND scope_type='price_book' \
             AND scope_id=$1 AND status='draft' ORDER BY id DESC LIMIT 1",
        )
        .bind(book_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?
        {
            tx.commit().await.map_err(storage_error)?;
            return load_change_set(&self.pool, existing).await;
        }
        let published = sqlx::query_as::<_, (i64, String)>(
            "SELECT id,reference_id FROM price_book_versions WHERE price_book_id=$1 \
             AND status='published' ORDER BY version DESC LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?;
        let now = Utc::now();
        let change_set_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,created_at,updated_at) \
             VALUES('retail_price','price_book',$1,$2,'draft',$3,$4,$5,$6,$6) RETURNING id",
        )
        .bind(book_id.to_string())
        .bind(format!("Retail prices: {book_name}"))
        .bind(published.as_ref().map(|(_, reference)| reference.as_str()).unwrap_or(""))
        .bind(settings.version.to_string())
        .bind(actor_user_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        if let Some((version_id, _)) = published {
            sqlx::query(
                "INSERT INTO change_set_items \
                 (change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) \
                 SELECT $1,CAST(i.public_model_id AS TEXT),'update',i.price,i.price,\
                        jsonb_build_object('publicModelID',i.public_model_id,'publicModelKey',m.model_id),$2,$2 \
                 FROM price_book_items i JOIN models m ON m.id=i.public_model_id AND m.deleted_at=0 \
                 WHERE i.price_book_version_id=$3",
            )
            .bind(change_set_id)
            .bind(now)
            .bind(version_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        }
        insert_event(
            &mut tx,
            change_set_id,
            "created",
            "user",
            Some(actor_user_id),
            json!({"priceBookID": book_id}),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        load_change_set(&self.pool, change_set_id).await
    }

    async fn save_retail_price_change_set_item(
        &self,
        actor_user_id: i64,
        input: gql::SaveRetailPriceChangeSetItemInput,
    ) -> Result<gql::ChangeSetItem, gql::ChangeSetError> {
        let change_set_id = parse_numeric_id(input.change_set_id.as_str(), "change set")?;
        let public_model_id = parse_numeric_id(input.public_model_id.as_str(), "public model")?;
        let price: ModelPrice = serde_json::from_value(input.price.0).map_err(|error| {
            gql::ChangeSetError::Invalid(format!("invalid model price: {error}"))
        })?;
        crate::wiring::validate_model_price(&price).map_err(|error| {
            gql::ChangeSetError::Invalid(format!("invalid model price: {error}"))
        })?;
        if price.items.is_empty() {
            return Err(gql::ChangeSetError::Invalid(
                "a retail price must contain at least one item".into(),
            ));
        }
        let after = serde_json::to_value(price).map_err(|error| {
            gql::ChangeSetError::Invalid(format!("cannot encode model price: {error}"))
        })?;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let change_set = lock_change_set(&mut tx, change_set_id).await?;
        if change_set.kind != "retail_price" || change_set.status != "draft" {
            return Err(gql::ChangeSetError::Invalid(
                "only a draft retail-price change set can be edited".into(),
            ));
        }
        let model_key = sqlx::query_scalar::<_, String>(
            "SELECT model_id FROM models WHERE id=$1 AND deleted_at=0",
        )
        .bind(public_model_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?
        .ok_or_else(|| {
            gql::ChangeSetError::Invalid(format!("public model {public_model_id} not found"))
        })?;
        let had_before = sqlx::query_scalar::<_, bool>(
            "SELECT before_snapshot IS NOT NULL FROM change_set_items \
             WHERE change_set_id=$1 AND item_key=$2",
        )
        .bind(change_set_id)
        .bind(public_model_id.to_string())
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage_error)?;
        let action = if had_before.unwrap_or(false) {
            "update"
        } else {
            "create"
        };
        let now = Utc::now();
        let item_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_set_items \
             (change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES($1,$2,$3,NULL,$4,$5,$6,$6) \
             ON CONFLICT(change_set_id,item_key) DO UPDATE SET action=$3,after_snapshot=$4,\
             source_snapshot=$5,validation_error=NULL,updated_at=$6 RETURNING id",
        )
        .bind(change_set_id)
        .bind(public_model_id.to_string())
        .bind(action)
        .bind(SqlJson(after))
        .bind(SqlJson(json!({"publicModelID": public_model_id, "publicModelKey": model_key})))
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        sqlx::query("UPDATE change_sets SET updated_at=$2 WHERE id=$1")
            .bind(change_set_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        insert_event(
            &mut tx,
            change_set_id,
            "item_saved",
            "user",
            Some(actor_user_id),
            json!({"itemKey": public_model_id.to_string()}),
            now,
        )
        .await?;
        let item = load_item_in_tx(&mut tx, item_id).await?;
        tx.commit().await.map_err(storage_error)?;
        item.into_graphql()
    }

    async fn submit_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let change_set = lock_change_set(&mut tx, id).await?;
        if change_set.status != "draft" {
            return Err(gql::ChangeSetError::Invalid(format!(
                "change set {id} is {}, only drafts can be submitted",
                change_set.status
            )));
        }
        let item_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM change_set_items WHERE change_set_id=$1 \
             AND validation_error IS NULL AND (\
                 (action='delete' AND before_snapshot IS NOT NULL AND after_snapshot IS NULL) OR \
                 (action IN ('create','update') AND after_snapshot IS NOT NULL)\
             )",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage_error)?;
        if item_count == 0 {
            return Err(gql::ChangeSetError::Invalid(
                "an empty change set cannot be submitted".into(),
            ));
        }
        let now = Utc::now();
        if change_set.kind == "provider_price" {
            sqlx::query(
                "WITH superseded AS (\
                   UPDATE change_sets candidate \
                   SET status='superseded',validation_error=NULL,reviewed_by=$2,reviewed_at=$3,\
                       review_note=$4,updated_at=$3 \
                   WHERE candidate.kind='provider_price' AND candidate.id<>$1 \
                     AND candidate.scope_type=$5 AND candidate.scope_id=$6 \
                     AND candidate.status IN ('pending_review','invalid') \
                     AND EXISTS(\
                       SELECT 1 FROM change_set_items manual_item \
                       JOIN change_set_items candidate_item \
                         ON candidate_item.item_key=manual_item.item_key \
                        AND candidate_item.change_set_id=candidate.id \
                       WHERE manual_item.change_set_id=$1 \
                         AND manual_item.source_snapshot->>'source'='manual' \
                         AND COALESCE(candidate_item.source_snapshot->>'source','observed')<>'manual'\
                     ) \
                   RETURNING candidate.id) \
                 INSERT INTO change_set_events(change_set_id,event_type,actor_type,actor_id,detail,created_at) \
                 SELECT id,'superseded','user',$2,jsonb_build_object('supersededByChangeSetID',$1),$3 \
                 FROM superseded",
            )
            .bind(id)
            .bind(actor_user_id)
            .bind(now)
            .bind(format!("superseded by manual change set {id}"))
            .bind(&change_set.scope_type)
            .bind(&change_set.scope_id)
            .execute(&mut *tx)
            .await
            .map_err(storage_error)?;
        }
        sqlx::query(
            "UPDATE change_sets SET status='pending_review',submitted_by=$2,submitted_at=$3,\
             updated_at=$3 WHERE id=$1",
        )
        .bind(id)
        .bind(actor_user_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;
        insert_event(
            &mut tx,
            id,
            "submitted",
            "user",
            Some(actor_user_id),
            json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        load_change_set(&self.pool, id).await
    }

    async fn approve_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
        review_note: Option<String>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let review_note = normalize_note(review_note);
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let change_set = lock_change_set(&mut tx, id).await?;
        if change_set.status != "pending_review" {
            return Err(gql::ChangeSetError::Invalid(format!(
                "change set {id} is {}, only pending change sets can be approved",
                change_set.status
            )));
        }
        let applied = {
            let mut apply_tx = tx.begin().await.map_err(storage_error)?;
            let result = match change_set.kind.as_str() {
                "provider_price" => {
                    apply_provider_price(&mut apply_tx, &change_set, actor_user_id).await
                }
                "model_mapping" => apply_model_mapping(&mut apply_tx, &change_set).await,
                "retail_price" => {
                    apply_retail_price(&mut apply_tx, &change_set, actor_user_id).await
                }
                other => Err(ApplyFailure::Invalid(format!(
                    "unsupported change set kind {other}"
                ))),
            };
            match result {
                Ok(target) => {
                    apply_tx.commit().await.map_err(storage_error)?;
                    Ok(target)
                }
                Err(error) => {
                    apply_tx.rollback().await.map_err(storage_error)?;
                    Err(error)
                }
            }
        };
        let (target_type, target_id) = match applied {
            Ok(target) => target,
            Err(ApplyFailure::Superseded(message)) => {
                let now = Utc::now();
                mark_reviewed(
                    &mut tx,
                    id,
                    "superseded",
                    actor_user_id,
                    review_note.as_deref(),
                    None,
                    now,
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    "superseded",
                    "user",
                    Some(actor_user_id),
                    json!({"reason": message}),
                    now,
                )
                .await?;
                tx.commit().await.map_err(storage_error)?;
                return Err(gql::ChangeSetError::Invalid(message));
            }
            Err(ApplyFailure::Invalid(message)) => {
                let now = Utc::now();
                mark_invalid(
                    &mut tx,
                    id,
                    actor_user_id,
                    review_note.as_deref(),
                    &message,
                    now,
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    "validation_failed",
                    "user",
                    Some(actor_user_id),
                    json!({"reason": message}),
                    now,
                )
                .await?;
                tx.commit().await.map_err(storage_error)?;
                return Err(gql::ChangeSetError::Invalid(message));
            }
            Err(ApplyFailure::Storage(message)) => {
                return Err(gql::ChangeSetError::Storage(message));
            }
        };
        let now = Utc::now();
        mark_reviewed(
            &mut tx,
            id,
            "applied",
            actor_user_id,
            review_note.as_deref(),
            Some((&target_type, &target_id)),
            now,
        )
        .await?;
        insert_event(
            &mut tx,
            id,
            "applied",
            "user",
            Some(actor_user_id),
            json!({"targetType": target_type, "targetID": target_id}),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        load_change_set(&self.pool, id).await
    }

    async fn reject_change_set(
        &self,
        actor_user_id: i64,
        id: ID,
        review_note: Option<String>,
    ) -> Result<gql::ChangeSet, gql::ChangeSetError> {
        let id = parse_numeric_id(id.as_str(), "change set")?;
        let review_note = normalize_note(review_note);
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        let change_set = lock_change_set(&mut tx, id).await?;
        if !matches!(change_set.status.as_str(), "pending_review" | "invalid") {
            return Err(gql::ChangeSetError::Invalid(format!(
                "change set {id} is {}, only pending or invalid change sets can be rejected",
                change_set.status
            )));
        }
        let now = Utc::now();
        mark_reviewed(
            &mut tx,
            id,
            "rejected",
            actor_user_id,
            review_note.as_deref(),
            None,
            now,
        )
        .await?;
        insert_event(
            &mut tx,
            id,
            "rejected",
            "user",
            Some(actor_user_id),
            json!({}),
            now,
        )
        .await?;
        tx.commit().await.map_err(storage_error)?;
        load_change_set(&self.pool, id).await
    }
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
    let row = sqlx::query_as::<_, ChangeSetRow>(&format!(
        "SELECT {CHANGE_SET_COLUMNS} FROM change_sets WHERE id=$1"
    ))
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
    let item_rows = sqlx::query_as::<_, ChangeSetItemRow>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items \
         WHERE change_set_id=ANY($1) ORDER BY change_set_id,id"
    ))
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
    sqlx::query_as::<_, ChangeSetRow>(&format!(
        "SELECT {CHANGE_SET_COLUMNS} FROM change_sets WHERE id=$1 FOR UPDATE"
    ))
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
    sqlx::query_as::<_, ChangeSetItemRow>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items WHERE change_set_id=$1 ORDER BY id"
    ))
    .bind(change_set_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| ApplyFailure::Storage(error.to_string()))
}

async fn load_item_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    item_id: i64,
) -> Result<ChangeSetItemRow, gql::ChangeSetError> {
    sqlx::query_as::<_, ChangeSetItemRow>(&format!(
        "SELECT {CHANGE_SET_ITEM_COLUMNS} FROM change_set_items WHERE id=$1"
    ))
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
    let channel_settings = sqlx::query_scalar::<_, SqlJson<Value>>(
        "SELECT settings FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_failure)?
    .ok_or_else(|| ApplyFailure::Invalid("channel not found".into()))?;
    let channel_settings = serde_json::from_value::<
        conduit_core::objects::channel_settings::ChannelSettings,
    >(channel_settings.0)
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
        if !is_manual {
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
        let parsed: ModelPrice = serde_json::from_value(proposed.clone())
            .map_err(|error| ApplyFailure::Invalid(format!("invalid provider price: {error}")))?;
        crate::wiring::validate_model_price(&parsed)
            .map_err(|error| ApplyFailure::Invalid(format!("invalid provider price: {error}")))?;
        if parsed.items.is_empty() {
            return Err(ApplyFailure::Invalid("provider price is empty".into()));
        }
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
                &settings.currency,
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
                &settings.currency,
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
            &settings.currency,
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
    Ok(("channel".into(), channel_id.to_string()))
}

async fn apply_model_mapping(
    tx: &mut Transaction<'_, Postgres>,
    change_set: &ChangeSetRow,
) -> Result<(String, String), ApplyFailure> {
    let channel_id = parse_i64(&change_set.scope_id, "channel")?;
    let channel_exists = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db_failure)?;
    if channel_exists.is_none() {
        return Err(ApplyFailure::Superseded(
            "mapping channel is no longer available".into(),
        ));
    }
    let items = load_items_in_tx(tx, change_set.id).await?;
    if items.is_empty() {
        return Err(ApplyFailure::Invalid(
            "model mapping change set is empty".into(),
        ));
    }
    let mut last_route_id = None;
    for item in items {
        let after = item
            .after_snapshot
            .as_ref()
            .map(|value| &value.0)
            .ok_or_else(|| ApplyFailure::Invalid("model mapping payload is missing".into()))?;
        let deployment_id = after
            .get("deploymentID")
            .and_then(Value::as_i64)
            .ok_or_else(|| ApplyFailure::Invalid("deploymentID is missing".into()))?;
        let public_model = after
            .get("publicModel")
            .and_then(Value::as_object)
            .ok_or_else(|| ApplyFailure::Invalid("publicModel is missing".into()))?;
        let model_key = required_string(public_model, "modelID")?;
        let deployment_channel = sqlx::query_scalar::<_, i64>(
            "SELECT channel_id FROM upstream_model_deployments WHERE id=$1 AND status='enabled' FOR UPDATE",
        )
        .bind(deployment_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_failure)?;
        if deployment_channel != Some(channel_id) {
            return Err(ApplyFailure::Superseded(format!(
                "upstream deployment {deployment_id} is no longer available"
            )));
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::BIGINT)")
            .bind(&model_key)
            .execute(&mut **tx)
            .await
            .map_err(db_failure)?;
        let existing = sqlx::query(
            "SELECT id,status,developer,\"type\",name,\"group\" FROM models \
             WHERE model_id=$1 AND deleted_at=0 LIMIT 1",
        )
        .bind(&model_key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db_failure)?;
        let current_snapshot = match existing.as_ref() {
            Some(row) => {
                let model_id: i64 = row.get("id");
                let route_status = sqlx::query_scalar::<_, String>(
                    "SELECT status FROM model_routes WHERE public_model_id=$1 AND deployment_id=$2",
                )
                .bind(model_id)
                .bind(deployment_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_failure)?;
                Some(json!({
                    "id": model_id,
                    "status": row.get::<String, _>("status"),
                    "developer": row.get::<String, _>("developer"),
                    "type": row.get::<String, _>("type"),
                    "name": row.get::<String, _>("name"),
                    "group": row.get::<String, _>("group"),
                    "routeStatus": route_status,
                }))
            }
            None => None,
        };
        let expected_snapshot = item.before_snapshot.as_ref().map(|value| value.0.clone());
        if current_snapshot != expected_snapshot {
            return Err(ApplyFailure::Superseded(format!(
                "public model or route for {model_key} changed after staging"
            )));
        }
        let public_model_id = match existing {
            Some(row) if row.get::<String, _>("status") == "archived" => {
                return Err(ApplyFailure::Invalid(format!(
                    "public model {model_key} is archived"
                )));
            }
            Some(row) => row.get("id"),
            None => {
                let developer = required_string(public_model, "developer")?;
                let model_type = required_string(public_model, "type")?;
                let name = required_string(public_model, "name")?;
                let group = required_string(public_model, "group")?;
                let conflict = sqlx::query_scalar::<_, String>(
                    "SELECT model_id FROM models WHERE name=$1 AND deleted_at=0 LIMIT 1",
                )
                .bind(&name)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db_failure)?;
                if conflict.is_some_and(|key| key != model_key) {
                    return Err(ApplyFailure::Invalid(format!(
                        "public model name {name} is already used"
                    )));
                }
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO models \
                     (developer,model_id,\"type\",name,icon,\"group\",model_card,settings,status,remark,created_at,updated_at,deleted_at) \
                     VALUES($1,$2,$3,$4,'',$5,'{}'::jsonb,'{}'::jsonb,'enabled',$6,now(),now(),0) RETURNING id",
                )
                .bind(developer)
                .bind(&model_key)
                .bind(model_type)
                .bind(name)
                .bind(group)
                .bind(format!("Created from approved model mapping change set {}", change_set.id))
                .fetch_one(&mut **tx)
                .await
                .map_err(db_failure)?
            }
        };
        let route_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO model_routes(public_model_id,deployment_id,status,created_at,updated_at) \
             VALUES($1,$2,'enabled',now(),now()) ON CONFLICT(public_model_id,deployment_id) \
             DO UPDATE SET status='enabled',updated_at=now() RETURNING id",
        )
        .bind(public_model_id)
        .bind(deployment_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(db_failure)?;
        last_route_id = Some(route_id);
    }
    sqlx::query("UPDATE channels SET updated_at=now() WHERE id=$1 AND deleted_at=0")
        .bind(channel_id)
        .execute(&mut **tx)
        .await
        .map_err(db_failure)?;
    Ok((
        "model_route".into(),
        last_route_id.unwrap_or_default().to_string(),
    ))
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
        let price = item
            .after_snapshot
            .map(|value| value.0)
            .ok_or_else(|| ApplyFailure::Invalid("retail price is missing".into()))?;
        let parsed: ModelPrice = serde_json::from_value(price.clone())
            .map_err(|error| ApplyFailure::Invalid(format!("invalid retail price: {error}")))?;
        crate::wiring::validate_model_price(&parsed)
            .map_err(|error| ApplyFailure::Invalid(format!("invalid retail price: {error}")))?;
        if parsed.items.is_empty() {
            return Err(ApplyFailure::Invalid(format!(
                "retail price for model {public_model_id} is empty"
            )));
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

async fn insert_event(
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
        other => Err(gql::ChangeSetError::Query(format!(
            "unknown change set kind {other}"
        ))),
    }
}

fn parse_status(value: &str) -> Result<gql::ChangeSetStatus, gql::ChangeSetError> {
    match value {
        "draft" => Ok(gql::ChangeSetStatus::Draft),
        "pending_review" => Ok(gql::ChangeSetStatus::PendingReview),
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
            price: conduit_admin_graphql::model_ext::ModelPriceInput {
                items: vec![conduit_admin_graphql::model_ext::ModelPriceItemInput {
                    item_code: conduit_admin_graphql::request_usage::PriceItemCode::PromptTokens,
                    pricing: conduit_admin_graphql::model_ext::PricingInput {
                        mode: conduit_admin_graphql::model_ext::PricingMode::UsagePerUnit,
                        flat_fee: None,
                        usage_per_unit: Some(conduit_admin_graphql::scalars::DecimalScalar(
                            Decimal::ONE,
                        )),
                        usage_tiered: None,
                    },
                    prompt_write_cache_variants: None,
                }],
            },
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
            ("superseded".into(), Some(7)),
            "a submitted manual proposal must supersede an overlapping observed proposal"
        );
        let applied = adapter
            .approve_change_set(8, draft.id, Some("manual prices verified".into()))
            .await?;
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
                .is_some_and(|error| error.contains("usagePerUnit is required"))
        );
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
        })))
        .bind(now)
        .execute(&database.pool)
        .await?;

        let adapter = PgChangeSetAdapter::new(database.pool.clone());
        let applied = adapter
            .approve_change_set(7, change_set_id.to_string().into(), Some("verified".into()))
            .await?;
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

//! Persisted formulas use formal procurement prices and only produce retail drafts.
use std::collections::BTreeMap;

use async_graphql::{ID, Json};
use conduit_admin_graphql::price_formula as gql;
use conduit_core::objects::{price_formula::PriceFormula, pricing::ModelPrice};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Row, Transaction, types::Json as SqlJson};

#[derive(Clone)]
pub(crate) struct PgPriceFormulaAdapter {
    pool: PgPool,
}
impl PgPriceFormulaAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(FromRow)]
struct FormulaRow {
    id: i64,
    price_book_id: i64,
    public_model_id: i64,
    deployment_id: i64,
    expression: String,
    parameters: SqlJson<Value>,
    enabled: bool,
    revision: i64,
    created_by: i64,
    status: String,
    last_error: Option<String>,
    last_fingerprint: Option<String>,
    last_change_set_id: Option<i64>,
}
impl FormulaRow {
    fn graphql(self) -> gql::RetailPriceFormula {
        gql::RetailPriceFormula {
            id: ID(self.id.to_string()),
            price_book_id: ID(self.price_book_id.to_string()),
            public_model_id: ID(format!("gid://conduit/Model/{}", self.public_model_id)),
            deployment_id: ID(self.deployment_id.to_string()),
            expression: self.expression,
            parameters: Json(self.parameters.0),
            enabled: self.enabled,
            revision: self.revision.to_string(),
            status: self.status,
            last_error: self.last_error,
            last_fingerprint: self.last_fingerprint,
            last_change_set_id: self.last_change_set_id.map(|id| ID(id.to_string())),
        }
    }
}
fn numeric(value: &str) -> Result<i64, String> {
    // Model IDs use the existing global-ID representation, never the public key.
    let value = value.rsplit('/').next().unwrap_or(value);
    let id = value.parse::<i64>().map_err(|_| "invalid object id")?;
    if id <= 0 {
        return Err("invalid object id".into());
    }
    Ok(id)
}
fn parameters(value: &Value) -> Result<BTreeMap<String, String>, String> {
    value
        .as_object()
        .ok_or("parameters must be a JSON object of decimal strings")?
        .iter()
        .map(|(name, value)| {
            Ok((
                name.clone(),
                value
                    .as_str()
                    .ok_or("constant values must be decimal strings")?
                    .into(),
            ))
        })
        .collect()
}
fn digest(value: &Value) -> String {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    format!(
        "{:x}",
        Sha256::digest(canonical(value).to_string().as_bytes())
    )
}
fn storage(error: sqlx::Error) -> String {
    format!("formula storage failed: {error}")
}

struct Evaluation {
    price: Value,
    source_price: Value,
    source: Value,
    fingerprint: String,
    currency: String,
    base_reference: String,
    before: Option<Value>,
    settings_version: String,
}

async fn evaluate(
    tx: &mut Transaction<'_, Postgres>,
    book: i64,
    model: i64,
    deployment: i64,
    expression: &str,
    constants: &Value,
) -> Result<Evaluation, String> {
    let formula = PriceFormula::parse(expression, &parameters(constants)?)?;
    let settings =
        crate::usage_charge_settler_postgres::load_accounting_settings_from(&mut **tx).await?;
    let row = sqlx::query(
        "SELECT m.model_id,m.name,m.\"type\",m.settings,r.id AS route_id,r.status AS route_status, \
         d.channel_id,d.upstream_model_id,d.variant,d.status AS deployment_status, \
         c.name AS channel_name,c.status AS channel_status,c.settings AS channel_settings, \
         b.currency,b.status AS book_status,p.id AS price_id,p.currency_code,p.price,p.reference_id \
         FROM models m JOIN model_routes r ON r.public_model_id=m.id AND r.deployment_id=$3 \
         JOIN upstream_model_deployments d ON d.id=r.deployment_id \
         JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
         JOIN price_books b ON b.id=$1 \
         LEFT JOIN channel_model_prices p ON p.channel_id=d.channel_id AND p.model_id=d.upstream_model_id AND p.deleted_at=0 \
         WHERE m.id=$2 AND m.deleted_at=0 AND m.status='enabled'",
    ).bind(book).bind(model).bind(deployment).fetch_optional(&mut **tx).await.map_err(storage)?
        .ok_or("SOURCE_UNAVAILABLE: formula requires an enabled public model and an explicit routed deployment")?;
    for field in [
        "route_status",
        "deployment_status",
        "channel_status",
        "book_status",
    ] {
        if row.get::<String, _>(field) != "enabled" {
            return Err(format!("SOURCE_UNAVAILABLE: {field} is not enabled"));
        }
    }
    let currency: String = row.get("currency");
    let source_currency: Option<String> = row.get("currency_code");
    if !currency.eq_ignore_ascii_case(&settings.accounting_currency)
        || source_currency
            .as_deref()
            .is_none_or(|c| !c.eq_ignore_ascii_case(&currency))
    {
        return Err("SOURCE_UNAVAILABLE: formal procurement price and book must use the accounting currency".into());
    }
    let source_price: Value = row
        .get::<Option<Value>, _>("price")
        .ok_or("SOURCE_UNAVAILABLE: formal procurement price is missing")?;
    let approved_source = sqlx::query_scalar::<_, Value>(
        "SELECT item.source_snapshot FROM pricing_change_audits audit \
         JOIN change_set_items item ON item.change_set_id=audit.source_change_set_id \
         WHERE audit.operation='apply_provider_price_change_set' AND audit.entity_id=$1 \
         AND audit.after_snapshot->>'referenceId'=$2 AND item.item_key=$3 \
         ORDER BY audit.id DESC LIMIT 1",
    )
    .bind(format!(
        "{}:{}",
        row.get::<i64, _>("channel_id"),
        row.get::<String, _>("upstream_model_id")
    ))
    .bind(row.get::<Option<String>, _>("reference_id"))
    .bind(row.get::<String, _>("upstream_model_id"))
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    if let Some(source) = approved_source
        .as_ref()
        .filter(|source| source["source"].as_str() != Some("manual"))
    {
        let channel: conduit_core::objects::channel_settings::ChannelSettings =
            serde_json::from_value(row.get::<Value, _>("channel_settings"))
                .map_err(|_| "SOURCE_UNAVAILABLE: invalid channel billing configuration")?;
        crate::wiring_postgres_pricing_admission::validate_channel_billing_snapshot(
            source,
            &channel.billing_currency,
            channel.recharge_multiplier,
        )?;
        crate::wiring_postgres_pricing_admission::validate_approved_conversion(source, &settings)?;
    }
    let parsed: ModelPrice =
        serde_json::from_value(source_price.clone()).map_err(|_| "invalid procurement price")?;
    crate::wiring::validate_model_price(&parsed)?;
    let result = formula.apply(&parsed)?;
    crate::wiring_postgres_pricing_admission::validate_price_coverage(
        &result,
        request_type(&row.get::<String, _>("type")),
    )?;

    let base_reference: String = sqlx::query_scalar("SELECT reference_id FROM price_book_versions WHERE price_book_id=$1 AND status='published' ORDER BY version DESC LIMIT 1")
        .bind(book).fetch_optional(&mut **tx).await.map_err(storage)?.unwrap_or_default();
    let before = sqlx::query_scalar::<_, Value>("SELECT i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.reference_id=$2 AND i.public_model_id=$3")
        .bind(book).bind(&base_reference).bind(model).fetch_optional(&mut **tx).await.map_err(storage)?;
    let price = conduit_core::objects::retail_price_plan::RetailPriceDocument::replace_global(
        before.as_ref(),
        result,
    )?;
    let source = json!({
        "publicModelID":model,"publicModelKey":row.get::<String,_>("model_id"),"displayName":row.get::<String,_>("name"),
        "modelType":row.get::<String,_>("type"),"modelSettingsFingerprint":digest(&row.get::<Value,_>("settings")),
        "deploymentID":deployment,"routeID":row.get::<i64,_>("route_id"),"channelID":row.get::<i64,_>("channel_id"),
        "channelName":row.get::<String,_>("channel_name"),"upstreamModelID":row.get::<String,_>("upstream_model_id"),
        "variant":row.get::<String,_>("variant"),"channelSettingsFingerprint":digest(&row.get::<Value,_>("channel_settings")),
        "procurementPriceID":row.get::<Option<i64>,_>("price_id"),"procurementReferenceID":row.get::<Option<String>,_>("reference_id"),
        "procurementPrice":source_price,"accountingSettings":settings,"priceBookID":book,"currency":currency,
        "procurementSourceFingerprint":approved_source.as_ref().map(digest),
        "baseReference":base_reference,"basePrice":before,"expression":expression,"parameters":constants,
        "algorithm":"decimal-base-price-v1","precision":12,"rounding":"half-away-from-zero"
    });
    let fingerprint = digest(&source);
    Ok(Evaluation {
        price,
        source_price,
        source,
        fingerprint,
        currency,
        base_reference,
        before,
        settings_version: settings.version.to_string(),
    })
}
fn request_type(kind: &str) -> conduit_llm::RequestType {
    match kind {
        "embedding" => conduit_llm::RequestType::Embedding,
        "image_generation" => conduit_llm::RequestType::Image,
        "video_generation" => conduit_llm::RequestType::Video,
        "rerank" => conduit_llm::RequestType::Rerank,
        _ => conduit_llm::RequestType::Chat,
    }
}

async fn retire(
    tx: &mut Transaction<'_, Postgres>,
    formula_id: i64,
    reason: &str,
) -> Result<(), String> {
    let ids: Vec<i64> = sqlx::query_scalar("UPDATE change_sets SET status='superseded',reviewed_at=now(),validation_error=NULL,updated_at=now() WHERE formula_id=$1 AND status IN ('draft','pending_review','approved') RETURNING id")
        .bind(formula_id).fetch_all(&mut **tx).await.map_err(storage)?;
    for id in ids {
        sqlx::query("INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at) VALUES($1,'formula_superseded','system',$2,now())")
            .bind(id).bind(SqlJson(json!({"reason":reason}))).execute(&mut **tx).await.map_err(storage)?;
    }
    Ok(())
}

async fn generate(
    tx: &mut Transaction<'_, Postgres>,
    row: &FormulaRow,
    evaluation: Evaluation,
) -> Result<(), String> {
    crate::wiring_governance::authorize(tx, row.created_by, "write_commercialization").await?;
    let pending = sqlx::query_as::<_, (i64, String)>("SELECT id,status FROM change_sets WHERE formula_id=$1 AND status IN ('draft','pending_review','approved') ORDER BY id DESC LIMIT 1")
        .bind(row.id).fetch_optional(&mut **tx).await.map_err(storage)?;
    if row.last_fingerprint.as_deref() == Some(&evaluation.fingerprint) && pending.is_some() {
        return Ok(());
    }
    // Only a draft owned by this formula may be retired. Manually edited items
    // detach that ownership in the existing save-item path.
    retire(tx, row.id, "formula inputs changed").await?;
    if evaluation.before.as_ref() == Some(&evaluation.price) {
        sqlx::query("UPDATE retail_price_formulas SET status='NO_CHANGE',last_error=NULL,last_fingerprint=$2,updated_at=now() WHERE id=$1")
            .bind(row.id).bind(&evaluation.fingerprint).execute(&mut **tx).await.map_err(storage)?;
        return Ok(());
    }
    let id: i64 = sqlx::query_scalar("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,base_revision,source_revision,created_by,formula_id,created_at,updated_at) VALUES($1,'retail_price','price_book',$2,$3,'draft',$4,$5,$6,$7,now(),now()) RETURNING id")
        .bind(uuid::Uuid::new_v4()).bind(row.price_book_id.to_string()).bind(format!("Formula base retail price: {}", evaluation.source["publicModelKey"].as_str().unwrap_or_default()))
        .bind(&evaluation.base_reference).bind(&evaluation.settings_version).bind(row.created_by).bind(row.id).fetch_one(&mut **tx).await.map_err(storage)?;
    let mut snapshot = evaluation.source;
    snapshot["formulaID"] = json!(row.id);
    snapshot["formulaRevision"] = json!(row.revision);
    snapshot["inputFingerprint"] = json!(evaluation.fingerprint);
    sqlx::query("INSERT INTO change_set_items(change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,now(),now())")
        .bind(id).bind(row.public_model_id.to_string()).bind(if evaluation.before.is_some() { "update" } else { "create" })
        .bind(evaluation.before.map(SqlJson)).bind(SqlJson(evaluation.price)).bind(SqlJson(snapshot))
        .execute(&mut **tx).await.map_err(storage)?;
    sqlx::query("INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at) VALUES($1,'created','system',$2,now())")
        .bind(id).bind(SqlJson(json!({"origin":"AUTOMATIC_SYNC","formulaID":row.id,"formulaRevision":row.revision,"inputFingerprint":evaluation.fingerprint})))
        .execute(&mut **tx).await.map_err(storage)?;
    sqlx::query("UPDATE retail_price_formulas SET status='DRAFT_READY',last_error=NULL,last_fingerprint=$2,last_change_set_id=$3,updated_at=now() WHERE id=$1")
        .bind(row.id).bind(evaluation.fingerprint).bind(id).execute(&mut **tx).await.map_err(storage)?;
    Ok(())
}

/// Every formal procurement applier calls this in its existing transaction.
/// Observations never call it; it cannot publish or borrow the provider actor.
pub(crate) async fn refresh_channel(
    tx: &mut Transaction<'_, Postgres>,
    channel: i64,
) -> Result<(), String> {
    let rows = sqlx::query_as::<_, FormulaRow>("SELECT f.* FROM retail_price_formulas f JOIN upstream_model_deployments d ON d.id=f.deployment_id WHERE d.channel_id=$1 AND f.enabled ORDER BY f.id FOR UPDATE OF f")
        .bind(channel).fetch_all(&mut **tx).await.map_err(storage)?;
    refresh_rows(tx, rows).await
}
pub(crate) async fn refresh_book(
    tx: &mut Transaction<'_, Postgres>,
    book: i64,
) -> Result<(), String> {
    let rows = sqlx::query_as::<_, FormulaRow>("SELECT * FROM retail_price_formulas WHERE price_book_id=$1 AND enabled ORDER BY id FOR UPDATE")
        .bind(book).fetch_all(&mut **tx).await.map_err(storage)?;
    refresh_rows(tx, rows).await
}
async fn refresh_rows(
    tx: &mut Transaction<'_, Postgres>,
    rows: Vec<FormulaRow>,
) -> Result<(), String> {
    for row in rows {
        let evaluated = evaluate(
            tx,
            row.price_book_id,
            row.public_model_id,
            row.deployment_id,
            &row.expression,
            &row.parameters.0,
        )
        .await;
        let result = match evaluated {
            Ok(evaluation) => generate(tx, &row, evaluation).await,
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            retire(tx, row.id, &error).await?;
            sqlx::query("UPDATE retail_price_formulas SET status='ERROR',last_error=$2,updated_at=now() WHERE id=$1")
                .bind(row.id).bind(error).execute(&mut **tx).await.map_err(storage)?;
        }
    }
    Ok(())
}

/// Recheck the exact frozen source and result in the publication transaction.
pub(crate) async fn validate_derived(
    tx: &mut Transaction<'_, Postgres>,
    book: i64,
    model: i64,
    source: &Value,
    proposed: &Value,
) -> Result<(), String> {
    let Some(id) = source.get("formulaID").and_then(Value::as_i64) else {
        return Ok(());
    };
    let row = sqlx::query_as::<_, FormulaRow>(
        "SELECT * FROM retail_price_formulas WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?
    .ok_or("SOURCE_CHANGED: formula was removed")?;
    if !row.enabled
        || row.price_book_id != book
        || row.public_model_id != model
        || source["formulaRevision"].as_i64() != Some(row.revision)
    {
        return Err("SOURCE_CHANGED: formula binding or revision changed".into());
    }
    crate::wiring_governance::authorize(tx, row.created_by, "write_commercialization").await?;
    let current = evaluate(
        tx,
        book,
        model,
        row.deployment_id,
        &row.expression,
        &row.parameters.0,
    )
    .await?;
    if source["inputFingerprint"].as_str() != Some(&current.fingerprint)
        || proposed != &current.price
    {
        return Err("SOURCE_CHANGED: formula source, base, settings or result changed".into());
    }
    Ok(())
}

#[async_trait::async_trait]
impl gql::PriceFormulaServices for PgPriceFormulaAdapter {
    async fn list(&self, actor: i64, book: &str) -> Result<Vec<gql::RetailPriceFormula>, String> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        crate::wiring_governance::authorize(&mut tx, actor, "read_commercialization").await?;
        let rows = sqlx::query_as::<_, FormulaRow>(
            "SELECT * FROM retail_price_formulas WHERE price_book_id=$1 ORDER BY id",
        )
        .bind(numeric(book)?)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(rows.into_iter().map(FormulaRow::graphql).collect())
    }
    async fn preview(
        &self,
        actor: i64,
        input: gql::RetailPriceFormulaInput,
    ) -> Result<gql::RetailPriceFormulaPreview, String> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        crate::wiring_governance::authorize(&mut tx, actor, "read_commercialization").await?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage)?;
        let value = evaluate(
            &mut tx,
            numeric(input.price_book_id.as_str())?,
            numeric(input.public_model_id.as_str())?,
            numeric(input.deployment_id.as_str())?,
            &input.expression,
            &input.parameters.0,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        Ok(gql::RetailPriceFormulaPreview {
            price: Json(value.price),
            source_price: Json(value.source_price),
            currency: value.currency,
            fingerprint: value.fingerprint,
            source: Json(value.source),
        })
    }
    async fn save(
        &self,
        actor: i64,
        input: gql::RetailPriceFormulaInput,
    ) -> Result<gql::RetailPriceFormula, String> {
        PriceFormula::parse(&input.expression, &parameters(&input.parameters.0)?)?;
        let book = numeric(input.price_book_id.as_str())?;
        let model = numeric(input.public_model_id.as_str())?;
        let deployment = numeric(input.deployment_id.as_str())?;
        let mut tx = self.pool.begin().await.map_err(storage)?;
        crate::wiring_governance::authorize(&mut tx, actor, "write_commercialization").await?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage)?;
        let existing = sqlx::query_as::<_, FormulaRow>("SELECT * FROM retail_price_formulas WHERE price_book_id=$1 AND public_model_id=$2 FOR UPDATE")
            .bind(book).bind(model).fetch_optional(&mut *tx).await.map_err(storage)?;
        if existing
            .as_ref()
            .map_or("0".into(), |row| row.revision.to_string())
            != input.expected_revision
        {
            return Err("REVISION_CONFLICT: formula changed; reload before saving".into());
        }
        // Disabling works even when the previous source is gone. Enabling or
        // changing a formula validates the same source as server preview.
        if !input.enabled
            && existing
                .as_ref()
                .is_none_or(|row| row.deployment_id != deployment)
        {
            let routed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_routes r JOIN upstream_model_deployments d ON d.id=r.deployment_id JOIN channels c ON c.id=d.channel_id JOIN models m ON m.id=r.public_model_id WHERE r.public_model_id=$1 AND r.deployment_id=$2 AND r.status='enabled' AND d.status='enabled' AND c.status='enabled' AND c.deleted_at=0 AND m.status='enabled' AND m.deleted_at=0)")
                .bind(model).bind(deployment).fetch_one(&mut *tx).await.map_err(storage)?;
            if !routed {
                return Err(
                    "SOURCE_UNAVAILABLE: the explicit source must belong to an enabled route"
                        .into(),
                );
            }
        }
        let evaluated = if input.enabled {
            Some(
                evaluate(
                    &mut tx,
                    book,
                    model,
                    deployment,
                    &input.expression,
                    &input.parameters.0,
                )
                .await?,
            )
        } else {
            None
        };
        let unchanged = existing.as_ref().is_some_and(|row| {
            row.deployment_id == deployment
                && row.expression == input.expression
                && row.parameters.0 == input.parameters.0
                && row.enabled == input.enabled
                && row.created_by == actor
        });
        let row = if unchanged {
            existing.ok_or("formula configuration unavailable")?
        } else {
            if let Some(row) = &existing {
                retire(&mut tx, row.id, "formula configuration changed").await?;
            }
            sqlx::query_as::<_, FormulaRow>("INSERT INTO retail_price_formulas(price_book_id,public_model_id,deployment_id,expression,parameters,enabled,created_by) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(price_book_id,public_model_id) DO UPDATE SET deployment_id=EXCLUDED.deployment_id,expression=EXCLUDED.expression,parameters=EXCLUDED.parameters,enabled=EXCLUDED.enabled,created_by=EXCLUDED.created_by,revision=retail_price_formulas.revision+1,status='DISABLED',last_error=NULL,last_fingerprint=NULL,updated_at=now() RETURNING *")
                .bind(book).bind(model).bind(deployment).bind(&input.expression).bind(SqlJson(input.parameters.0)).bind(input.enabled).bind(actor).fetch_one(&mut *tx).await.map_err(storage)?
        };
        if let Some(evaluation) = evaluated {
            generate(&mut tx, &row, evaluation).await?;
        } else {
            retire(&mut tx, row.id, "formula disabled").await?;
            sqlx::query("UPDATE retail_price_formulas SET status='DISABLED',last_error=NULL,updated_at=now() WHERE id=$1").bind(row.id).execute(&mut *tx).await.map_err(storage)?;
        }
        let result =
            sqlx::query_as::<_, FormulaRow>("SELECT * FROM retail_price_formulas WHERE id=$1")
                .bind(row.id)
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?
                .graphql();
        tx.commit().await.map_err(storage)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiring_postgres_change_sets::{PgChangeSetAdapter, packages};
    use conduit_admin_graphql::change_package::{
        ChangePackageServices as _, ChangeSetAvailableAction as Action, PackageViewer,
    };
    use conduit_admin_graphql::model_ext::{
        ModelPriceInput, ModelPriceItemInput, PricingInput, PricingMode, SaveChannelModelPriceInput,
    };
    use conduit_admin_graphql::price_formula::PriceFormulaServices as _;
    use conduit_admin_graphql::{request_usage::PriceItemCode, scalars::DecimalScalar};
    use rust_decimal::Decimal;

    fn procurement(amount: i64) -> SaveChannelModelPriceInput {
        SaveChannelModelPriceInput {
            model_id: "model-a".into(),
            currency_code: "CNY".into(),
            price: ModelPriceInput {
                items: [
                    PriceItemCode::PromptTokens,
                    PriceItemCode::CompletionTokens,
                    PriceItemCode::PromptCachedTokens,
                    PriceItemCode::PromptWriteCachedTokens,
                ]
                .into_iter()
                .map(|item_code| ModelPriceItemInput {
                    item_code,
                    pricing: PricingInput {
                        mode: PricingMode::UsagePerUnit,
                        usage_per_unit: Some(DecimalScalar(Decimal::from(amount))),
                        flat_fee: None,
                        usage_tiered: None,
                    },
                    prompt_write_cache_variants: None,
                })
                .collect(),
            },
        }
    }
    struct Fixture {
        db: crate::postgres_test_support::IsolatedPostgres,
        book: i64,
        model: i64,
        second_model: i64,
        channel: i64,
        deployment: i64,
    }
    impl Fixture {
        async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let db = crate::postgres_test_support::IsolatedPostgres::new(dsn).await?;
            // Revoking the author must leave two eligible owners for the
            // existing governance reviewer-protection invariant.
            db.owners(&[1, 2, 4]).await?;
            sqlx::query("INSERT INTO users(id,email,password,status,is_owner,scopes) VALUES(3,'reader@formula.invalid','fixture','activated',false,'[\"read_commercialization\"]')").execute(&db.pool).await?;
            sqlx::query("INSERT INTO systems(key,value) VALUES('system_governance_settings','{\"enabled\":false}')").execute(&db.pool).await?;
            let book = sqlx::query_scalar("INSERT INTO price_books(name,currency,status,is_default) VALUES('Formula book','CNY','enabled',true) RETURNING id").fetch_one(&db.pool).await?;
            let channel = sqlx::query_scalar("INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model,settings) VALUES('openai','Channel 1','enabled','{}','[\"model-a\"]','model-a','{\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}') RETURNING id").fetch_one(&db.pool).await?;
            let deployment = sqlx::query_scalar("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,status,source) VALUES($1,'model-a','Model A','enabled','test') RETURNING id").bind(channel).fetch_one(&db.pool).await?;
            let mut models = Vec::new();
            for key in ["channel1.modelA", "channel2.modelA"] {
                models.push(sqlx::query_scalar::<_,i64>("INSERT INTO models(model_id,name,developer,type,\"group\",icon,model_card,settings,status) VALUES($1,'Model A','test','chat','test','','{}','{}','enabled') RETURNING id").bind(key).fetch_one(&db.pool).await?);
            }
            let model = models[0];
            let second_model = models[1];
            sqlx::query("INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES($1,$2,'enabled')").bind(model).bind(deployment).execute(&db.pool).await?;
            packages::save_native_provider(&db.pool, 1, channel, vec![procurement(2)]).await?;
            sqlx::query("UPDATE systems SET value='{\"enabled\":true,\"reviewKinds\":[\"retail_price\"],\"allowSelfReview\":false,\"legacyReviewScopes\":false}' WHERE key='system_governance_settings'").execute(&db.pool).await?;
            eprintln!("isolated PostgreSQL formula fixture migrated and formal price seeded");
            Ok(Self {
                db,
                book,
                model,
                second_model,
                channel,
                deployment,
            })
        }
        fn input(&self) -> gql::RetailPriceFormulaInput {
            gql::RetailPriceFormulaInput {
                price_book_id: ID(self.book.to_string()),
                public_model_id: ID(format!("gid://conduit/Model/{}", self.model)),
                deployment_id: ID(self.deployment.to_string()),
                expression: "cost * markup".into(),
                parameters: Json(json!({"markup":"1.5"})),
                enabled: true,
                expected_revision: "0".into(),
            }
        }
        async fn current(&self, id: ID) -> conduit_admin_graphql::change_package::ChangeSetDetail {
            PgChangeSetAdapter::new(self.db.pool.clone())
                .get_detail(
                    PackageViewer {
                        actor_user_id: 1,
                        can_write: true,
                    },
                    id,
                )
                .await
                .unwrap()
                .unwrap()
        }
        async fn publish(&self, id: ID) -> Result<(), Box<dyn std::error::Error>> {
            let adapter = PgChangeSetAdapter::new(self.db.pool.clone());
            for (actor, action) in [
                (1, Action::Submit),
                (2, Action::Approve),
                (2, Action::Publish),
            ] {
                let revision = self.current(id.clone()).await.summary.revision;
                adapter
                    .transition(actor, id.clone(), revision, action, None)
                    .await?;
            }
            Ok(())
        }
        async fn price(&self, model: i64) -> Result<Option<Value>, sqlx::Error> {
            sqlx::query_scalar("SELECT i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.status='published' AND i.public_model_id=$2")
                .bind(self.book).bind(model).fetch_optional(&self.db.pool).await
        }
    }

    #[tokio::test]
    async fn postgres_formula_persistence_preview_governance_and_formal_price_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = Fixture::new(&dsn).await?;
        let adapter = PgPriceFormulaAdapter::new(fixture.db.pool.clone());
        let input = fixture.input();
        assert!(
            adapter
                .save(3, input.clone())
                .await
                .unwrap_err()
                .contains("FORBIDDEN")
        );
        let preview = adapter.preview(3, input.clone()).await?;
        assert_eq!(preview.price.0["items"][0]["pricing"]["usagePerUnit"], "3");
        let (first, second) = tokio::join!(
            adapter.save(1, input.clone()),
            adapter.save(1, input.clone())
        );
        assert!(
            first.is_ok() ^ second.is_ok(),
            "concurrent creation must admit exactly one revision"
        );
        let saved = first.or(second)?;
        let draft = saved.last_change_set_id.clone().unwrap();
        assert_eq!(saved.status, "DRAFT_READY");
        assert!(
            fixture.price(fixture.model).await?.is_none(),
            "draft cannot affect billing"
        );
        let generated: Value = sqlx::query_scalar(
            "SELECT after_snapshot FROM change_set_items WHERE change_set_id=$1",
        )
        .bind(numeric(draft.as_str())?)
        .fetch_one(&fixture.db.pool)
        .await?;
        assert_eq!(
            generated, preview.price.0,
            "preview and generation share the server algorithm"
        );
        let persisted = PgPriceFormulaAdapter::new(fixture.db.pool.clone())
            .list(1, &fixture.book.to_string())
            .await?;
        assert_eq!(persisted[0].expression, input.expression);
        let mut same = input.clone();
        same.expected_revision = saved.revision.clone();
        assert_eq!(
            adapter.save(1, same.clone()).await?.last_change_set_id,
            Some(draft.clone())
        );
        assert!(
            adapter
                .save(1, input)
                .await
                .unwrap_err()
                .contains("REVISION_CONFLICT")
        );
        let packages_adapter = PgChangeSetAdapter::new(fixture.db.pool.clone());
        assert!(
            packages_adapter
                .transition(
                    1,
                    draft.clone(),
                    fixture.current(draft.clone()).await.summary.revision,
                    Action::Publish,
                    None
                )
                .await
                .is_err(),
            "review cannot be bypassed"
        );
        fixture.publish(draft.clone()).await?;
        assert_eq!(fixture.price(fixture.model).await?.unwrap(), generated);
        assert!(
            fixture.price(fixture.second_model).await?.is_none(),
            "same display name has an independent price"
        );
        assert_eq!(
            adapter.list(1, &fixture.book.to_string()).await?[0].status,
            "NO_CHANGE"
        );
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM change_sets WHERE kind='retail_price'")
                .fetch_one(&fixture.db.pool)
                .await?;
        assert_eq!(count, 1, "self publication must not create a loop");

        // An observed provider price is not a formal price update.
        sqlx::query("INSERT INTO provider_price_snapshots(channel_id,adapter_id,adapter_version,attempted_endpoints,status,warnings,started_at,observed_at) VALUES($1,'test','1','[]','success','[]',now(),now())").bind(fixture.channel).execute(&fixture.db.pool).await?;
        assert_eq!(
            adapter.list(1, &fixture.book.to_string()).await?[0].last_change_set_id,
            Some(draft)
        );
        let (first, second) = tokio::join!(
            packages::save_native_provider(
                &fixture.db.pool,
                2,
                fixture.channel,
                vec![procurement(4)]
            ),
            packages::save_native_provider(
                &fixture.db.pool,
                2,
                fixture.channel,
                vec![procurement(4)]
            )
        );
        first?;
        second?;
        let updated = adapter.list(1, &fixture.book.to_string()).await?.remove(0);
        assert_eq!(updated.status, "DRAFT_READY");
        let second_draft = updated.last_change_set_id.clone().unwrap();
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap()["items"][0]["pricing"]["usagePerUnit"],
            "3"
        );
        packages::save_native_provider(&fixture.db.pool, 2, fixture.channel, vec![procurement(4)])
            .await?;
        assert_eq!(
            adapter.list(1, &fixture.book.to_string()).await?[0].last_change_set_id,
            Some(second_draft.clone())
        );
        fixture.publish(second_draft).await?;
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap()["items"][0]["pricing"]["usagePerUnit"],
            "6"
        );
        packages::save_native_provider(&fixture.db.pool, 2, fixture.channel, vec![]).await?;
        assert_eq!(
            adapter.list(1, &fixture.book.to_string()).await?[0].status,
            "ERROR",
            "formal deletion invalidates the fixed source"
        );
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap()["items"][0]["pricing"]["usagePerUnit"],
            "6",
            "source deletion keeps the last published price"
        );
        fixture.db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_formula_preserves_group_branches_in_preview_draft_publish_and_refresh()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = Fixture::new(&dsn).await?;
        let group = uuid::Uuid::new_v4().to_string();
        let price = json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"flat_fee","flatFee":"5"}}]});
        let document = json!({"schema":"group-conditional-price-v1","global":price,"groups":[{"modelGroupID":group,"rules":[{"id":"busy","label":"busy","condition":"hour >= 9 && hour < 18","price":price}],"base":price}]});
        use conduit_admin_graphql::change_set::ChangeSetServices as _;
        let change_sets = PgChangeSetAdapter::new(fixture.db.pool.clone());
        let seed = change_sets
            .create_retail_price_change_set(1, ID(fixture.book.to_string()))
            .await?;
        change_sets
            .save_retail_price_change_set_item(
                1,
                conduit_admin_graphql::change_set::SaveRetailPriceChangeSetItemInput {
                    expected_revision: String::new(),
                    change_set_id: seed.id.clone(),
                    public_model_id: ID(fixture.model.to_string()),
                    price: Json(document.clone()),
                },
            )
            .await?;
        fixture.publish(seed.id).await?;
        assert_eq!(fixture.price(fixture.model).await?, Some(document.clone()));
        let adapter = PgPriceFormulaAdapter::new(fixture.db.pool.clone());
        let preview = adapter.preview(1, fixture.input()).await?;
        assert_eq!(preview.price.0["groups"], document["groups"]);
        let saved = adapter.save(1, fixture.input()).await?;
        let draft = saved.last_change_set_id.unwrap();
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap(),
            document,
            "draft must not publish"
        );
        fixture.publish(draft).await?;
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap(),
            preview.price.0
        );
        packages::save_native_provider(&fixture.db.pool, 2, fixture.channel, vec![procurement(4)])
            .await?;
        let updated = adapter.list(1, &fixture.book.to_string()).await?.remove(0);
        fixture.publish(updated.last_change_set_id.unwrap()).await?;
        assert_eq!(
            fixture.price(fixture.model).await?.unwrap()["groups"],
            document["groups"]
        );
        fixture.db.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_formula_source_drift_disable_manual_override_and_author_permissions()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let fixture = Fixture::new(&dsn).await?;
        let adapter = PgPriceFormulaAdapter::new(fixture.db.pool.clone());
        let saved = adapter.save(1, fixture.input()).await?;
        let draft = saved.last_change_set_id.clone().unwrap();
        let current = fixture.current(draft.clone()).await;
        let packages_adapter = PgChangeSetAdapter::new(fixture.db.pool.clone());
        let manual = json!({"items":[{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"0.25"}}]});
        packages_adapter
            .save_retail_item(
                1,
                conduit_admin_graphql::change_set::SaveRetailPriceChangeSetItemInput {
                    change_set_id: draft.clone(),
                    expected_revision: current.summary.revision,
                    public_model_id: ID(fixture.model.to_string()),
                    price: Json(manual.clone()),
                },
            )
            .await?;
        let source: Value = sqlx::query_scalar(
            "SELECT source_snapshot FROM change_set_items WHERE change_set_id=$1",
        )
        .bind(numeric(draft.as_str())?)
        .fetch_one(&fixture.db.pool)
        .await?;
        assert!(
            source.get("formulaID").is_none(),
            "manual edit clears derived metadata"
        );
        let mut disabled = fixture.input();
        disabled.expected_revision = saved.revision.clone();
        disabled.enabled = false;
        let disabled = adapter.save(1, disabled).await?;
        assert_eq!(
            fixture.current(draft.clone()).await.summary.status,
            conduit_admin_graphql::change_set::ChangeSetStatus::Draft,
            "disable must not retire a manual override"
        );
        fixture.publish(draft).await?;
        assert_eq!(fixture.price(fixture.model).await?, Some(manual.clone()));
        let mut enabled = fixture.input();
        enabled.expected_revision = disabled.revision;
        let enabled = adapter.save(1, enabled).await?;
        let stale = enabled.last_change_set_id.clone().unwrap();
        sqlx::query("UPDATE model_routes SET status='disabled' WHERE public_model_id=$1")
            .bind(fixture.model)
            .execute(&fixture.db.pool)
            .await?;
        assert_eq!(
            fixture.current(stale.clone()).await.summary.status,
            conduit_admin_graphql::change_set::ChangeSetStatus::Superseded
        );
        assert_eq!(
            adapter.list(1, &fixture.book.to_string()).await?[0].status,
            "SOURCE_CHANGED"
        );
        assert_eq!(fixture.price(fixture.model).await?, Some(manual.clone()));
        assert!(
            adapter
                .preview(1, fixture.input())
                .await
                .unwrap_err()
                .contains("SOURCE_UNAVAILABLE")
        );
        sqlx::query("UPDATE model_routes SET status='enabled' WHERE public_model_id=$1")
            .bind(fixture.model)
            .execute(&fixture.db.pool)
            .await?;
        let mut resumed = fixture.input();
        resumed.expected_revision = enabled.revision;
        let resumed = adapter.save(1, resumed).await?;
        let source: Value = sqlx::query_scalar(
            "SELECT source_snapshot FROM change_set_items WHERE change_set_id=$1",
        )
        .bind(numeric(
            resumed.last_change_set_id.as_ref().unwrap().as_str(),
        )?)
        .fetch_one(&fixture.db.pool)
        .await?;
        let proposed: Value = sqlx::query_scalar(
            "SELECT after_snapshot FROM change_set_items WHERE change_set_id=$1",
        )
        .bind(numeric(
            resumed.last_change_set_id.as_ref().unwrap().as_str(),
        )?)
        .fetch_one(&fixture.db.pool)
        .await?;
        let mut tx = fixture.db.pool.begin().await?;
        // A persisted fingerprint is evidence, not authority to change a result.
        assert!(
            validate_derived(
                &mut tx,
                fixture.book,
                fixture.model,
                &source,
                &json!({"items":[]})
            )
            .await
            .is_err()
        );
        assert!(
            validate_derived(&mut tx, fixture.book, fixture.model, &source, &proposed)
                .await
                .is_ok()
        );
        tx.rollback().await?;
        sqlx::query("UPDATE users SET is_owner=false,scopes='[]' WHERE id=1")
            .execute(&fixture.db.pool)
            .await?;
        packages::save_native_provider(&fixture.db.pool, 2, fixture.channel, vec![procurement(8)])
            .await?;
        let failed = adapter.list(2, &fixture.book.to_string()).await?.remove(0);
        assert_eq!(failed.status, "ERROR");
        assert!(
            failed.last_error.unwrap().contains("FORBIDDEN"),
            "provider publisher must not lend formula author permission"
        );
        assert_eq!(fixture.price(fixture.model).await?, Some(manual));
        fixture.db.cleanup().await?;
        Ok(())
    }
}

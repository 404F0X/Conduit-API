//! PostgreSQL persistence for advisory upstream price observations.
//!
//! These snapshots never mutate `channel_model_prices`. Runtime accounting
//! continues to use administrator-confirmed prices; observations only give the
//! operations UI an auditable upstream-cost history.

use std::collections::BTreeMap;

use conduit_core::objects::channel_settings::ChannelSettings;
use conduit_core::objects::money::AccountingSettings;
use conduit_core::objects::pricing::{
    ModelPrice, ModelPriceItem, PRICING_MODE_FLAT_FEE, PRICING_MODE_USAGE_PER_UNIT, Pricing,
    price_item_code,
};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, types::Json};

use crate::wiring_postgres_provider_quota::{NewApiModelPricingSnapshot, NewApiPricingSnapshot};

const ADAPTER_ID: &str = "new_api";
const ADAPTER_VERSION: &str = "1";
const OBSERVATION_INTERVAL_HOURS: i64 = 3;
const PRICING_LOCK_NAMESPACE: i64 = 0x5052_4943_4500_0000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PriceIdentity {
    model: String,
    group: String,
    billing: String,
}

#[derive(Debug, Clone)]
struct ComparableRow {
    identity: PriceIdentity,
    fingerprint: String,
    comparable_price: Option<rust_decimal::Decimal>,
}

#[derive(Debug, Clone)]
struct PriceConversionContext {
    billing_currency: Option<String>,
    recharge_multiplier: Option<Decimal>,
    accounting_settings: Option<AccountingSettings>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct ConvertedPrices {
    input_per_million: Option<Decimal>,
    output_per_million: Option<Decimal>,
    cache_read_per_million: Option<Decimal>,
    cache_write_per_million: Option<Decimal>,
    flat_per_request: Option<Decimal>,
    error: Option<String>,
}

pub(crate) async fn observation_due(pool: &PgPool, channel_id: i64) -> Result<bool, String> {
    let latest = sqlx::query_scalar::<_, chrono::DateTime<chrono::Utc>>(
        "SELECT observed_at FROM provider_price_snapshots \
         WHERE channel_id=$1 ORDER BY observed_at DESC,id DESC LIMIT 1",
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(latest.is_none_or(|observed| {
        chrono::Utc::now() - observed >= chrono::Duration::hours(OBSERVATION_INTERVAL_HOURS)
    }))
}

pub(crate) async fn record_pricing_failure(
    pool: &PgPool,
    channel_id: i64,
    attempted_endpoints: &[&str],
    error: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO provider_price_snapshots \
         (channel_id,adapter_id,adapter_version,attempted_endpoints,status,error_message, \
          warnings,started_at,observed_at) \
         VALUES($1,$2,$3,$4,'failed',$5,'[]'::jsonb,$6,$6)",
    )
    .bind(channel_id)
    .bind(ADAPTER_ID)
    .bind(ADAPTER_VERSION)
    .bind(Json(json!(attempted_endpoints)))
    .bind(error)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|db_error| db_error.to_string())?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct PricingDiscoveryCommit {
    pub snapshot_id: i64,
    pub package: Option<crate::wiring_postgres_change_sets::packages::StagedPackage>,
}
pub(crate) async fn record_new_api_pricing_snapshot(
    pool: &PgPool,
    channel_id: i64,
    endpoint: &str,
    snapshot: &NewApiPricingSnapshot,
) -> Result<i64, String> {
    record_new_api_pricing_snapshot_with_context(
        pool,
        channel_id,
        endpoint,
        snapshot,
        &crate::wiring_postgres_change_sets::OperationContext::automatic(),
        None,
        None,
    )
    .await
    .map(|c| c.snapshot_id)
}
pub(crate) async fn record_new_api_pricing_snapshot_with_context(
    pool: &PgPool,
    channel_id: i64,
    endpoint: &str,
    snapshot: &NewApiPricingSnapshot,
    context: &crate::wiring_postgres_change_sets::OperationContext,
    fence: Option<&crate::wiring_postgres_channel_discovery::DiscoveryFence>,
    rebuild: Option<&conduit_admin_graphql::channel_discovery::RebuildObservedPricePackagesInput>,
) -> Result<PricingDiscoveryCommit, String> {
    let group = snapshot.effective_groups.join(",");
    let normalized = json!({
        "pricingVersion": snapshot.pricing_version,
        "accountGroup": snapshot.account_group,
        "effectiveGroups": snapshot.effective_groups,
        "models": snapshot.models.iter().map(model_json).collect::<Vec<_>>(),
    });
    let payload_hash = sha256_hex(normalized.to_string().as_bytes());

    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    if let Some(fence) = fence {
        crate::wiring_postgres_channel_discovery::lock_fence(&mut tx, fence).await?;
    }
    // A channel's observations form one linear history. Serialize writers so
    // concurrent manual/scheduled probes cannot both diff from the same head.
    lock_provider_pricing_channel(&mut tx, channel_id).await?;
    sqlx::query("SELECT id FROM change_sets WHERE scope_type='channel' AND scope_id=$1 ORDER BY id FOR UPDATE").bind(channel_id.to_string()).fetch_all(&mut *tx).await.map_err(|e|e.to_string())?;
    crate::wiring::lock_accounting_currency_price_writes(&mut tx)
        .await
        .map_err(|_| "accounting price lock unavailable".to_string())?;
    sqlx::query("SELECT id FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE")
        .bind(channel_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| "channel is unavailable".to_string())?;
    let conversion = load_conversion_context(&mut tx, channel_id).await;

    let previous_snapshot_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM provider_price_snapshots \
         WHERE channel_id=$1 AND status='success' \
         ORDER BY observed_at DESC,id DESC LIMIT 1",
    )
    .bind(channel_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    let previous = match previous_snapshot_id {
        Some(id) => load_rows(&mut tx, id).await?,
        None => BTreeMap::new(),
    };

    let snapshot_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO provider_price_snapshots \
         (channel_id,adapter_id,adapter_version,primary_endpoint,attempted_endpoints, \
          pricing_version,raw_payload_sha256,status,warnings,started_at,observed_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,'success',$8,$9,$9) RETURNING id",
    )
    .bind(channel_id)
    .bind(ADAPTER_ID)
    .bind(ADAPTER_VERSION)
    .bind(endpoint)
    .bind(Json(json!([{"endpoint": endpoint, "status": "success"}])))
    .bind(&snapshot.pricing_version)
    .bind(payload_hash)
    .bind(Json(json!(snapshot.warnings)))
    .bind(snapshot.fetched_at)
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;

    let mut current = BTreeMap::new();
    let mut proposals = Vec::new();
    for model in &snapshot.models {
        let converted = conversion.convert(model);
        let row = comparable_row(model, &group, &conversion, &converted);
        let provider_price_row_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO provider_price_rows \
             (snapshot_id,channel_id,upstream_model_id,group_name,billing_kind,quality, \
              currency,group_ratio,input_per_million,output_per_million, \
              cache_read_per_million,cache_write_per_million,flat_per_request,reason, \
              raw_item_sha256,source_unit,billing_currency,recharge_multiplier, \
              accounting_currency,accounting_input_per_million, \
              accounting_output_per_million,accounting_cache_read_per_million, \
              accounting_cache_write_per_million,accounting_flat_per_request, \
              accounting_settings_version,conversion_error,provenance) \
             VALUES($1,$2,$3,$4,$5,$6,NULL,$7,$8,$9,$10,$11,$12,$13,$14, \
                    'CHANNEL_BALANCE_UNIT',$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25) RETURNING id",
        )
        .bind(snapshot_id)
        .bind(channel_id)
        .bind(&model.model_id)
        .bind(&group)
        .bind(&model.billing_kind)
        .bind(normalize_quality(&model.quality))
        .bind(decimal(model.group_ratio))
        .bind(decimal(model.input_per_million))
        .bind(decimal(model.output_per_million))
        .bind(decimal(model.cache_read_per_million))
        .bind(decimal(model.cache_write_per_million))
        .bind(decimal(model.flat_per_request))
        .bind(&model.reason)
        .bind(sha256_hex(model_json(model).to_string().as_bytes()))
        .bind(&conversion.billing_currency)
        .bind(decimal(conversion.recharge_multiplier))
        .bind(
            conversion
                .accounting_settings
                .as_ref()
                .map(|settings| settings.accounting_currency.as_str()),
        )
        .bind(decimal(converted.input_per_million))
        .bind(decimal(converted.output_per_million))
        .bind(decimal(converted.cache_read_per_million))
        .bind(decimal(converted.cache_write_per_million))
        .bind(decimal(converted.flat_per_request))
        .bind(
            conversion
                .accounting_settings
                .as_ref()
                .and_then(|settings| i64::try_from(settings.version).ok()),
        )
        .bind(&converted.error)
        .bind(Json(model.provenance.clone()))
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if model.provenance["adoptionBlocked"] != true
            && let Some(proposal) = provider_price_proposal(
                &mut tx,
                channel_id,
                snapshot_id,
                provider_price_row_id,
                model,
                &group,
                &conversion,
                &converted,
            )
            .await?
        {
            proposals.push(proposal);
        }
        current.insert(row.identity.clone(), row);
    }

    for (identity, row) in &current {
        match previous.get(identity) {
            None => {
                insert_event(
                    &mut tx,
                    channel_id,
                    previous_snapshot_id,
                    snapshot_id,
                    identity,
                    "added",
                    None,
                    None,
                    Some(&row.fingerprint),
                    snapshot.fetched_at,
                )
                .await?;
            }
            Some(old) if old.fingerprint != row.fingerprint => {
                let event = match (old.comparable_price, row.comparable_price) {
                    (Some(before), Some(after)) if after > before => "increased",
                    (Some(before), Some(after)) if after < before => "decreased",
                    _ => "changed",
                };
                insert_event(
                    &mut tx,
                    channel_id,
                    previous_snapshot_id,
                    snapshot_id,
                    identity,
                    event,
                    Some("normalized_price"),
                    Some(&old.fingerprint),
                    Some(&row.fingerprint),
                    snapshot.fetched_at,
                )
                .await?;
            }
            _ => {}
        }
    }
    for (identity, old) in &previous {
        if !current.contains_key(identity) {
            insert_event(
                &mut tx,
                channel_id,
                previous_snapshot_id,
                snapshot_id,
                identity,
                "removed",
                None,
                Some(&old.fingerprint),
                None,
                snapshot.fetched_at,
            )
            .await?;
        }
    }

    let package = crate::wiring_postgres_change_sets::packages::stage_observed(
        &mut tx,
        context,
        "provider_price",
        channel_id,
        "Upstream procurement price discovery",
        &snapshot_id.to_string(),
        &proposals,
        snapshot.fetched_at,
    )
    .await?;
    let commit = PricingDiscoveryCommit {
        snapshot_id,
        package,
    };
    if let Some(fence) = fence {
        crate::wiring_postgres_channel_discovery::finish_prices(
            &mut tx, fence, channel_id, &commit, snapshot, rebuild,
        )
        .await?;
    }
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(commit)
}

fn proposed_model_price(
    model: &NewApiModelPricingSnapshot,
    converted: &ConvertedPrices,
) -> Result<ModelPrice, String> {
    if model
        .provenance
        .get("issues")
        .and_then(Value::as_array)
        .is_some_and(|issues| issues.iter().any(|i| i["severity"] == "BLOCKING"))
    {
        return Err(model
            .reason
            .clone()
            .unwrap_or_else(|| "upstream pricing evidence requires correction".into()));
    }
    if let Some(error) = converted.error.as_ref() {
        return Err(error.clone());
    }
    if !matches!(normalize_quality(&model.quality), "exact" | "estimated") {
        return Err(model
            .reason
            .clone()
            .unwrap_or_else(|| format!("upstream price quality is {}", model.quality)));
    }
    let usage_item = |item_code: &str, value: Option<Decimal>| -> Result<ModelPriceItem, String> {
        let value = value.ok_or_else(|| format!("{item_code} price is required"))?;
        Ok(ModelPriceItem {
            item_code: item_code.to_string(),
            pricing: Pricing {
                mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                usage_per_unit: Some(value),
                ..Pricing::default()
            },
            ..ModelPriceItem::default()
        })
    };
    match model.billing_kind.as_str() {
        "token" => Ok(ModelPrice {
            items: vec![
                usage_item(price_item_code::USAGE, converted.input_per_million)?,
                usage_item(price_item_code::COMPLETION, converted.output_per_million)?,
                usage_item(
                    price_item_code::PROMPT_CACHED_TOKEN,
                    converted.cache_read_per_million,
                )?,
                usage_item(
                    price_item_code::WRITE_CACHED_TOKENS,
                    converted.cache_write_per_million,
                )?,
            ],
        }),
        "per_request" => Ok(ModelPrice {
            items: vec![ModelPriceItem {
                item_code: "request".to_string(),
                pricing: Pricing {
                    mode: PRICING_MODE_FLAT_FEE.to_string(),
                    flat_fee: Some(
                        converted
                            .flat_per_request
                            .ok_or_else(|| "flat request price is required".to_string())?,
                    ),
                    ..Pricing::default()
                },
                ..ModelPriceItem::default()
            }],
        }),
        other => Err(format!("unsupported upstream billing kind {other}")),
    }
}

#[allow(clippy::too_many_arguments)]
async fn provider_price_proposal(
    tx: &mut Transaction<'_, Postgres>,
    channel_id: i64,
    snapshot_id: i64,
    provider_price_row_id: i64,
    model: &NewApiModelPricingSnapshot,
    group: &str,
    conversion: &PriceConversionContext,
    converted: &ConvertedPrices,
) -> Result<Option<crate::wiring_postgres_change_sets::packages::Proposal>, String> {
    let proposed = if model.billing_kind == "tiered_expr" {
        model
            .provenance
            .get("billingExpression")
            .filter(|value| !value.is_null())
            .cloned()
            .ok_or_else(|| {
                model
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Expression is unsupported".into())
            })
            .and_then(|value| {
                let accounting = conversion
                    .accounting_settings
                    .as_ref()
                    .ok_or("Accounting settings are unavailable")?;
                accounting.real_to_accounting(Decimal::ONE, "USD")?;
                let price: conduit_core::objects::newapi_price::NewApiPrice =
                    serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
                price.compile()?;
                Ok(value)
            })
    } else {
        proposed_model_price(model, converted)
            .and_then(|price| serde_json::to_value(price).map_err(|error| error.to_string()))
    };
    let proposed_json = proposed.as_ref().ok().cloned();
    let current = sqlx::query_as::<_, (i64, String, Json<Value>, String)>(
        "SELECT id,currency_code,price,reference_id FROM channel_model_prices \
         WHERE channel_id=$1 AND model_id=$2 AND deleted_at=0 LIMIT 1",
    )
    .bind(channel_id)
    .bind(&model.model_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    let base_price_snapshot = current
        .as_ref()
        .map(|(id, currency, Json(price), reference_id)| {
            json!({
                "id": id,
                "currencyCode": currency,
                "price": price,
                "referenceID": reference_id,
            })
        });

    let mut source = comparable_object(model, conversion, converted);
    source.insert("snapshotID".into(), json!(snapshot_id));
    source.insert("providerPriceRowID".into(), json!(provider_price_row_id));
    source.insert("groupName".into(), json!(group));
    source.insert("billingKind".into(), json!(model.billing_kind));
    source.insert("quality".into(), json!(normalize_quality(&model.quality)));
    source.insert("source".into(), json!("observed"));
    source.insert("provenance".into(), model.provenance.clone());
    let source_price_snapshot = Value::Object(source);
    if proposed_json.as_ref().is_some_and(|price| {
        current
            .as_ref()
            .is_some_and(|(_, currency, Json(current), _)| {
                current == price
                    && (if model.billing_kind == "tiered_expr" {
                        currency == "USD"
                    } else {
                        conversion
                            .accounting_settings
                            .as_ref()
                            .is_some_and(|s| currency == &s.accounting_currency)
                    })
            })
    }) {
        return Ok(None);
    }
    Ok(Some(
        crate::wiring_postgres_change_sets::packages::Proposal {
            key: model.model_id.clone(),
            action: if current.is_some() {
                "update"
            } else {
                "create"
            }
            .into(),
            before: base_price_snapshot,
            after: proposed_json,
            source: source_price_snapshot,
            error: proposed.err(),
        },
    ))
}

pub(crate) async fn lock_provider_pricing_channel(
    tx: &mut Transaction<'_, Postgres>,
    channel_id: i64,
) -> Result<(), String> {
    let lock_key = PRICING_LOCK_NAMESPACE.wrapping_add(channel_id);
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut **tx)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn load_rows(
    tx: &mut Transaction<'_, Postgres>,
    snapshot_id: i64,
) -> Result<BTreeMap<PriceIdentity, ComparableRow>, String> {
    let rows = sqlx::query(
        "SELECT upstream_model_id,group_name,billing_kind,quality,group_ratio, \
         input_per_million,output_per_million,cache_read_per_million, \
         cache_write_per_million,flat_per_request,reason,source_unit, \
         billing_currency,recharge_multiplier,accounting_currency, \
         accounting_input_per_million,accounting_output_per_million, \
         accounting_cache_read_per_million,accounting_cache_write_per_million, \
         accounting_flat_per_request,accounting_settings_version,conversion_error \
         FROM provider_price_rows WHERE snapshot_id=$1",
    )
    .bind(snapshot_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    let mut result = BTreeMap::new();
    for row in rows {
        let identity = PriceIdentity {
            model: row.get("upstream_model_id"),
            group: row.get("group_name"),
            billing: row.get("billing_kind"),
        };
        let value = json!({
            "quality": row.get::<String, _>("quality"),
            "groupRatio": row.get::<Option<String>, _>("group_ratio"),
            "input": row.get::<Option<String>, _>("input_per_million"),
            "output": row.get::<Option<String>, _>("output_per_million"),
            "cacheRead": row.get::<Option<String>, _>("cache_read_per_million"),
            "cacheWrite": row.get::<Option<String>, _>("cache_write_per_million"),
            "flat": row.get::<Option<String>, _>("flat_per_request"),
            "reason": row.get::<Option<String>, _>("reason"),
            "sourceUnit": row.get::<String, _>("source_unit"),
            "billingCurrency": row.get::<Option<String>, _>("billing_currency"),
            "rechargeMultiplier": row.get::<Option<String>, _>("recharge_multiplier"),
            "accountingCurrency": row.get::<Option<String>, _>("accounting_currency"),
            "accountingInput": row.get::<Option<String>, _>("accounting_input_per_million"),
            "accountingOutput": row.get::<Option<String>, _>("accounting_output_per_million"),
            "accountingCacheRead": row.get::<Option<String>, _>("accounting_cache_read_per_million"),
            "accountingCacheWrite": row.get::<Option<String>, _>("accounting_cache_write_per_million"),
            "accountingFlat": row.get::<Option<String>, _>("accounting_flat_per_request"),
            "accountingSettingsVersion": row.get::<Option<i64>, _>("accounting_settings_version"),
            "conversionError": row.get::<Option<String>, _>("conversion_error"),
        });
        let comparable_price = value["accountingInput"]
            .as_str()
            .or_else(|| value["accountingFlat"].as_str())
            .and_then(|value| value.parse().ok());
        result.insert(
            identity.clone(),
            ComparableRow {
                identity,
                fingerprint: value.to_string(),
                comparable_price,
            },
        );
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn insert_event(
    tx: &mut Transaction<'_, Postgres>,
    channel_id: i64,
    from_snapshot_id: Option<i64>,
    to_snapshot_id: i64,
    identity: &PriceIdentity,
    event_type: &str,
    field_name: Option<&str>,
    from_value: Option<&str>,
    to_value: Option<&str>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO provider_price_change_events \
         (channel_id,from_snapshot_id,to_snapshot_id,upstream_model_id,group_name, \
          billing_kind,event_type,field_name,from_value,to_value,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(channel_id)
    .bind(from_snapshot_id)
    .bind(to_snapshot_id)
    .bind(&identity.model)
    .bind(&identity.group)
    .bind(&identity.billing)
    .bind(event_type)
    .bind(field_name)
    .bind(from_value)
    .bind(to_value)
    .bind(created_at)
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn comparable_row(
    model: &NewApiModelPricingSnapshot,
    group: &str,
    conversion: &PriceConversionContext,
    converted: &ConvertedPrices,
) -> ComparableRow {
    let value = Value::Object(comparable_object(model, conversion, converted));
    ComparableRow {
        identity: PriceIdentity {
            model: model.model_id.clone(),
            group: group.to_string(),
            billing: model.billing_kind.clone(),
        },
        fingerprint: value.to_string(),
        comparable_price: converted.input_per_million.or(converted.flat_per_request),
    }
}

async fn load_conversion_context(
    tx: &mut Transaction<'_, Postgres>,
    channel_id: i64,
) -> PriceConversionContext {
    let channel_settings = sqlx::query_scalar::<_, Json<Value>>(
        "SELECT COALESCE(settings, '{}'::jsonb) FROM channels WHERE id=$1 AND deleted_at=0",
    )
    .bind(channel_id)
    .fetch_optional(&mut **tx)
    .await;
    let (billing_currency, recharge_multiplier, mut errors) = match channel_settings {
        Ok(Some(Json(value))) => match serde_json::from_value::<ChannelSettings>(value) {
            Ok(settings) => {
                let currency = (!settings.billing_currency.trim().is_empty())
                    .then(|| settings.billing_currency.trim().to_ascii_uppercase());
                let mut errors = Vec::new();
                if currency.is_none() || settings.recharge_multiplier.is_none() {
                    errors.push(
                        "channel billing currency and recharge multiplier are required".into(),
                    );
                } else if settings.recharge_multiplier <= Some(Decimal::ZERO) {
                    errors.push("channel recharge multiplier must be positive".into());
                }
                (currency, settings.recharge_multiplier, errors)
            }
            Err(error) => (
                None,
                None,
                vec![format!("invalid channel settings: {error}")],
            ),
        },
        Ok(None) => (None, None, vec![format!("channel {channel_id} not found")]),
        Err(error) => (
            None,
            None,
            vec![format!("failed to load channel settings: {error}")],
        ),
    };
    let accounting_settings =
        match crate::usage_charge_settler_postgres::load_accounting_settings_from(&mut **tx).await {
            Ok(settings) => Some(settings),
            Err(error) => {
                errors.push(error);
                None
            }
        };
    PriceConversionContext {
        billing_currency,
        recharge_multiplier,
        accounting_settings,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

impl PriceConversionContext {
    fn convert(&self, model: &NewApiModelPricingSnapshot) -> ConvertedPrices {
        let Some(settings) = self.accounting_settings.as_ref() else {
            return ConvertedPrices::failed(
                self.error
                    .clone()
                    .unwrap_or_else(|| "accounting settings are unavailable".into()),
            );
        };
        let (Some(currency), Some(multiplier)) =
            (self.billing_currency.as_deref(), self.recharge_multiplier)
        else {
            return ConvertedPrices::failed(
                self.error
                    .clone()
                    .unwrap_or_else(|| "channel billing metadata is incomplete".into()),
            );
        };
        let convert = |amount: Option<Decimal>| -> Result<Option<Decimal>, String> {
            amount
                .map(|amount| settings.channel_units_to_accounting(amount, currency, multiplier))
                .transpose()
        };
        let result = (|| {
            Ok::<_, String>(ConvertedPrices {
                input_per_million: convert(model.input_per_million)?,
                output_per_million: convert(model.output_per_million)?,
                cache_read_per_million: convert(model.cache_read_per_million)?,
                cache_write_per_million: convert(model.cache_write_per_million)?,
                flat_per_request: convert(model.flat_per_request)?,
                error: None,
            })
        })();
        result.unwrap_or_else(ConvertedPrices::failed)
    }
}

impl ConvertedPrices {
    fn failed(error: String) -> Self {
        Self {
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            flat_per_request: None,
            error: Some(error),
        }
    }
}

fn comparable_object(
    model: &NewApiModelPricingSnapshot,
    conversion: &PriceConversionContext,
    converted: &ConvertedPrices,
) -> Map<String, Value> {
    let mut object = price_object(model);
    object.insert("sourceUnit".into(), json!("CHANNEL_BALANCE_UNIT"));
    object.insert("billingCurrency".into(), json!(conversion.billing_currency));
    object.insert(
        "rechargeMultiplier".into(),
        json!(decimal(conversion.recharge_multiplier)),
    );
    object.insert(
        "accountingCurrency".into(),
        json!(
            conversion
                .accounting_settings
                .as_ref()
                .map(|value| &value.accounting_currency)
        ),
    );
    object.insert(
        "accountingInput".into(),
        json!(decimal(converted.input_per_million)),
    );
    object.insert(
        "accountingOutput".into(),
        json!(decimal(converted.output_per_million)),
    );
    object.insert(
        "accountingCacheRead".into(),
        json!(decimal(converted.cache_read_per_million)),
    );
    object.insert(
        "accountingCacheWrite".into(),
        json!(decimal(converted.cache_write_per_million)),
    );
    object.insert(
        "accountingFlat".into(),
        json!(decimal(converted.flat_per_request)),
    );
    object.insert(
        "accountingSettingsVersion".into(),
        json!(
            conversion
                .accounting_settings
                .as_ref()
                .map(|value| value.version)
        ),
    );
    object.insert("conversionError".into(), json!(converted.error));
    object
}

fn model_json(model: &NewApiModelPricingSnapshot) -> Value {
    json!({
        "model": model.model_id,
        "billing": model.billing_kind,
        "price": price_json(model),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_model() -> NewApiModelPricingSnapshot {
        NewApiModelPricingSnapshot {
            provenance: json!({}),
            model_id: "model-a".into(),
            billing_kind: "token".into(),
            quality: "exact".into(),
            group_ratio: None,
            input_per_million: Some(Decimal::ONE),
            output_per_million: Some(Decimal::from(2)),
            cache_read_per_million: Some(Decimal::new(1, 1)),
            cache_write_per_million: Some(Decimal::new(2, 1)),
            flat_per_request: None,
            reason: None,
        }
    }

    fn converted(model: &NewApiModelPricingSnapshot) -> ConvertedPrices {
        ConvertedPrices {
            input_per_million: model.input_per_million,
            output_per_million: model.output_per_million,
            cache_read_per_million: model.cache_read_per_million,
            cache_write_per_million: model.cache_write_per_million,
            flat_per_request: model.flat_per_request,
            error: None,
        }
    }

    #[test]
    fn token_draft_requires_and_preserves_all_prompt_cache_prices() {
        let model = token_model();
        let price = proposed_model_price(&model, &converted(&model)).expect("valid price");

        let items = price
            .items
            .iter()
            .map(|item| (item.item_code.as_str(), item.pricing.usage_per_unit))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(items.get(price_item_code::USAGE), Some(&Some(Decimal::ONE)));
        assert_eq!(
            items.get(price_item_code::COMPLETION),
            Some(&Some(Decimal::from(2)))
        );
        assert_eq!(
            items.get(price_item_code::PROMPT_CACHED_TOKEN),
            Some(&Some(Decimal::new(1, 1)))
        );
        assert_eq!(
            items.get(price_item_code::WRITE_CACHED_TOKENS),
            Some(&Some(Decimal::new(2, 1)))
        );
    }

    #[test]
    fn token_draft_is_invalid_when_cache_write_price_is_missing() {
        let model = token_model();
        let mut values = converted(&model);
        values.cache_write_per_million = None;

        let error = proposed_model_price(&model, &values).expect_err("missing cache price");
        assert!(error.contains(price_item_code::WRITE_CACHED_TOKENS));
    }

    #[test]
    fn untrusted_upstream_quality_never_creates_an_approvable_price() {
        let mut model = token_model();
        model.quality = "unknown".into();
        model.reason = Some("provider omitted price metadata".into());

        let error = proposed_model_price(&model, &converted(&model)).expect_err("untrusted");
        assert_eq!(error, "provider omitted price metadata");
    }

    #[allow(clippy::too_many_arguments)]
    async fn stage_test_observation(
        tx: &mut Transaction<'_, Postgres>,
        channel: i64,
        snapshot: i64,
        row: i64,
        model: &NewApiModelPricingSnapshot,
        group: &str,
        conversion: &PriceConversionContext,
        converted: &ConvertedPrices,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), String> {
        let proposals = provider_price_proposal(
            tx, channel, snapshot, row, model, group, conversion, converted,
        )
        .await?
        .into_iter()
        .collect::<Vec<_>>();
        crate::wiring_postgres_change_sets::packages::stage_observed(
            tx,
            &crate::wiring_postgres_change_sets::OperationContext::automatic(),
            "provider_price",
            channel,
            "Provider price observation",
            &snapshot.to_string(),
            &proposals,
            now,
        )
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_new_observations_refresh_or_supersede_price_change_sets_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let conversion = PriceConversionContext {
            billing_currency: Some("CNY".into()),
            recharge_multiplier: Some(Decimal::ONE),
            accounting_settings: Some(AccountingSettings::default()),
            error: None,
        };
        let model = token_model();
        let converted = conversion.convert(&model);
        let mut tx = database.pool.begin().await?;
        let now = chrono::Utc::now();

        stage_test_observation(
            &mut tx,
            777,
            1001,
            2001,
            &model,
            "",
            &conversion,
            &converted,
            now,
        )
        .await?;
        stage_test_observation(
            &mut tx,
            777,
            1002,
            2002,
            &model,
            "",
            &conversion,
            &converted,
            now + chrono::Duration::seconds(1),
        )
        .await?;
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT COUNT(*),MIN(CAST(source_revision AS BIGINT)),MAX(CAST(source_revision AS BIGINT)) \
                 FROM change_sets cs WHERE kind='provider_price' AND scope_type='channel' \
                   AND scope_id='777' AND status='pending_review' AND EXISTS(\
                     SELECT 1 FROM change_set_items item \
                     WHERE item.change_set_id=cs.id AND item.item_key='model-a')"
            )
            .fetch_one(&mut *tx)
            .await?,
            (1, 1002, 1002)
        );

        let manual_change_set_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO change_sets \
             (kind,scope_type,scope_id,title,status,source_revision,submitted_at,created_at,updated_at) \
             VALUES('provider_price','channel','777','manual price','pending_review','1',$1,$1,$1) \
             RETURNING id",
        )
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO change_set_items \
             (change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES($1,'model-a','create','{}'::jsonb,'{\"source\":\"manual\"}'::jsonb,$2,$2)",
        )
        .bind(manual_change_set_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let invalid = ConvertedPrices::failed("upstream price is incomplete".into());
        stage_test_observation(
            &mut tx,
            777,
            1003,
            2003,
            &model,
            "",
            &conversion,
            &invalid,
            now + chrono::Duration::seconds(2),
        )
        .await?;
        assert_eq!(
            sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT COUNT(*) FILTER (WHERE status='pending_review'),\
                        COUNT(*) FILTER (WHERE status='superseded'),\
                        COUNT(*) FILTER (WHERE status='invalid') \
                 FROM change_sets cs WHERE kind='provider_price' AND scope_type='channel' \
                   AND scope_id='777' AND EXISTS(SELECT 1 FROM change_set_items item \
                     WHERE item.change_set_id=cs.id AND item.item_key='model-a')"
            )
            .fetch_one(&mut *tx)
            .await?,
            (1, 1, 1)
        );

        tx.rollback().await?;
        database.cleanup().await?;
        Ok(())
    }
}

fn price_json(model: &NewApiModelPricingSnapshot) -> Value {
    Value::Object(price_object(model))
}

fn price_object(model: &NewApiModelPricingSnapshot) -> Map<String, Value> {
    Map::from_iter([
        ("quality".into(), json!(normalize_quality(&model.quality))),
        ("groupRatio".into(), json!(decimal(model.group_ratio))),
        ("input".into(), json!(decimal(model.input_per_million))),
        ("output".into(), json!(decimal(model.output_per_million))),
        (
            "cacheRead".into(),
            json!(decimal(model.cache_read_per_million)),
        ),
        (
            "cacheWrite".into(),
            json!(decimal(model.cache_write_per_million)),
        ),
        ("flat".into(), json!(decimal(model.flat_per_request))),
        ("reason".into(), json!(model.reason)),
    ])
}

fn decimal(value: Option<rust_decimal::Decimal>) -> Option<String> {
    value.map(|value| value.normalize().to_string())
}

fn normalize_quality(value: &str) -> &str {
    match value {
        "unsupported" => "unavailable",
        value => value,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

//! Request-time procurement-price admission and theoretical cost scoring.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use conduit_core::error::{ConduitError, ErrorKind};
use conduit_core::objects::money::AccountingSettings;
use conduit_core::objects::pricing::{ModelPrice, PRICING_MODE_FLAT_FEE, price_item_code};
use conduit_llm::{RequestType, Usage};
use conduit_orchestrator::candidates::{
    CandidateRequest, ChannelModelsCandidate, FilterStage, SelectionDiagnostics, SelectionRejection,
};
use conduit_orchestrator::orchestrator::CandidateSource;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use sqlx::{PgPool, types::Json};

#[derive(Clone)]
pub(crate) struct PgPricingAdmissionCandidateSource {
    inner: Arc<dyn CandidateSource>,
    pool: PgPool,
}

impl PgPricingAdmissionCandidateSource {
    pub(crate) fn new(inner: Arc<dyn CandidateSource>, pool: PgPool) -> Self {
        Self { inner, pool }
    }
}

#[derive(Debug)]
struct StoredPrice {
    currency: String,
    price: Value,
    approved_source: Option<Value>,
    channel_billing_currency: String,
    channel_recharge_multiplier: Option<Decimal>,
}

#[async_trait]
impl CandidateSource for PgPricingAdmissionCandidateSource {
    async fn select(
        &self,
        request: &CandidateRequest,
    ) -> Result<Vec<ChannelModelsCandidate>, ConduitError> {
        self.select_with_diagnostics(request)
            .await
            .map(|(candidates, _)| candidates)
    }

    async fn select_with_diagnostics(
        &self,
        request: &CandidateRequest,
    ) -> Result<(Vec<ChannelModelsCandidate>, SelectionDiagnostics), ConduitError> {
        let (candidates, mut diagnostics) = self.inner.select_with_diagnostics(request).await?;
        if candidates.is_empty() {
            return Ok((candidates, diagnostics));
        }

        let accounting = crate::usage_charge_settler_postgres::load_accounting_settings(&self.pool)
            .await
            .map_err(configuration_error)?;
        let channel_ids = candidates
            .iter()
            .filter_map(|candidate| candidate.channel_id.parse::<i64>().ok())
            .collect::<Vec<_>>();
        let rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Json<Value>,
                Option<Json<Value>>,
                Json<Value>,
            ),
        >(
            "SELECT price.channel_id,price.model_id,price.currency_code,price.price, \
                    approved.source_snapshot,channel.settings \
             FROM channel_model_prices price \
             JOIN channels channel ON channel.id=price.channel_id AND channel.deleted_at=0 \
             LEFT JOIN LATERAL ( \
                 SELECT item.source_snapshot \
                 FROM change_sets cs JOIN change_set_items item ON item.change_set_id=cs.id \
                 WHERE cs.kind='provider_price' AND cs.status='applied' \
                   AND cs.scope_type='channel' AND cs.scope_id=CAST(price.channel_id AS TEXT) \
                   AND item.item_key=price.model_id AND item.after_snapshot=price.price \
                 ORDER BY cs.applied_at DESC NULLS LAST,cs.id DESC LIMIT 1 \
             ) approved ON TRUE \
             WHERE price.deleted_at=0 AND price.channel_id=ANY($1)",
        )
        .bind(&channel_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            configuration_error(format!("failed to load procurement prices: {error}"))
        })?;
        let prices = rows
            .into_iter()
            .map(
                |(
                    channel_id,
                    model_id,
                    currency,
                    Json(price),
                    approved_source,
                    Json(channel_settings),
                )| {
                    let settings = serde_json::from_value::<
                        conduit_core::objects::channel_settings::ChannelSettings,
                    >(channel_settings)
                    .map_err(|error| {
                        configuration_error(format!(
                            "channel {channel_id} has invalid billing settings: {error}"
                        ))
                    })?;
                    Ok((
                        (channel_id, model_id),
                        StoredPrice {
                            currency,
                            price,
                            approved_source: approved_source.map(|Json(value)| value),
                            channel_billing_currency: settings.billing_currency,
                            channel_recharge_multiplier: settings.recharge_multiplier,
                        },
                    ))
                },
            )
            .collect::<Result<BTreeMap<_, _>, ConduitError>>()?;

        let usage = estimated_usage(request);
        let mut admitted = Vec::new();
        for mut candidate in candidates {
            match candidate_cost(
                &candidate,
                request.request_type,
                &usage,
                &accounting,
                &prices,
            ) {
                Ok(cost) => {
                    candidate.theoretical_cost_accounting = Some(cost.normalize().to_string());
                    admitted.push(candidate);
                }
                Err(detail) => diagnostics.rejected.push(SelectionRejection {
                    stage: FilterStage::PricingAdmission,
                    channel_id: candidate.channel_id,
                    channel_name: candidate.channel_name,
                    detail,
                }),
            }
        }

        if admitted.is_empty() {
            return Err(configuration_error(
                "all eligible candidates have incomplete procurement pricing",
            ));
        }
        normalize_cost_scores(&mut admitted);
        let admitted_ids = admitted
            .iter()
            .map(|candidate| candidate.channel_id.as_str())
            .collect::<BTreeSet<_>>();
        diagnostics
            .selected
            .retain(|candidate| admitted_ids.contains(candidate.channel_id.as_str()));
        Ok((admitted, diagnostics))
    }
}

fn candidate_cost(
    candidate: &ChannelModelsCandidate,
    request_type: RequestType,
    usage: &Usage,
    accounting: &AccountingSettings,
    prices: &BTreeMap<(i64, String), StoredPrice>,
) -> Result<Decimal, String> {
    let channel_id = candidate
        .channel_id
        .parse::<i64>()
        .map_err(|_| "channel id is not a database id".to_string())?;
    // Candidate projection and outbound execution both select the first model.
    // Price exactly that model so regex-associated alternates cannot reject or
    // distort a route that will never execute them.
    let model = candidate
        .models
        .first()
        .ok_or_else(|| "candidate has no concrete upstream model".to_string())?;
    let key = (channel_id, model.actual_model.clone());
    let stored = prices
        .get(&key)
        .ok_or_else(|| format!("missing procurement price for model {}", model.actual_model))?;
    if let Some(source) = stored.approved_source.as_ref() {
        validate_channel_billing_snapshot(
            source,
            &stored.channel_billing_currency,
            stored.channel_recharge_multiplier,
        )?;
        validate_approved_conversion(source, accounting)?;
    }
    let price = serde_json::from_value::<ModelPrice>(stored.price.clone())
        .map_err(|error| format!("invalid procurement price JSON: {error}"))?;
    crate::wiring::validate_model_price(&price)
        .map_err(|error| format!("invalid procurement price: {error}"))?;
    validate_price_coverage(&price, request_type)?;
    let source_cost =
        conduit_services::usage_service::compute_usage_cost_full(Some(usage), &price).total;
    accounting
        .real_to_accounting(source_cost, &stored.currency)
        .map_err(|error| format!("procurement price conversion failed: {error}"))
}

pub(crate) fn validate_channel_billing_snapshot(
    source: &Value,
    current_currency: &str,
    current_recharge_multiplier: Option<Decimal>,
) -> Result<(), String> {
    let source_currency = source
        .get("billingCurrency")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "approved procurement price has no upstream recharge currency".to_string()
        })?;
    let source_multiplier = decimal_snapshot(source, "rechargeMultiplier")?
        .ok_or_else(|| "approved procurement price has no recharge multiplier".to_string())?;
    let current_currency = current_currency.trim();
    let Some(current_multiplier) = current_recharge_multiplier else {
        return Err(
            "procurement price requires review after channel billing conversion change".into(),
        );
    };
    if source_currency.eq_ignore_ascii_case(current_currency)
        && source_multiplier == current_multiplier
    {
        Ok(())
    } else {
        Err("procurement price requires review after channel billing conversion change".into())
    }
}

fn validate_approved_conversion(
    source: &Value,
    accounting: &AccountingSettings,
) -> Result<(), String> {
    let source_version = source
        .get("accountingSettingsVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "approved procurement price has no accounting settings version".to_string()
        })?;
    if source_version == accounting.version {
        return Ok(());
    }

    let source_accounting_currency = source
        .get("accountingCurrency")
        .and_then(Value::as_str)
        .ok_or_else(|| "approved procurement price has no accounting currency".to_string())?;
    if !source_accounting_currency.eq_ignore_ascii_case(&accounting.accounting_currency) {
        return Err("approved procurement price uses a different accounting currency".into());
    }
    let billing_currency = source
        .get("billingCurrency")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "approved procurement price has no upstream recharge currency".to_string()
        })?;
    let recharge_multiplier = decimal_snapshot(source, "rechargeMultiplier")?
        .ok_or_else(|| "approved procurement price has no recharge multiplier".to_string())?;

    let mut compared = false;
    for (native_field, accounting_field) in [
        ("input", "accountingInput"),
        ("output", "accountingOutput"),
        ("cacheRead", "accountingCacheRead"),
        ("cacheWrite", "accountingCacheWrite"),
        ("flat", "accountingFlat"),
    ] {
        let native = decimal_snapshot(source, native_field)?;
        let approved = decimal_snapshot(source, accounting_field)?;
        match (native, approved) {
            (None, None) => continue,
            (Some(native), Some(approved)) => {
                compared = true;
                let current = accounting
                    .channel_units_to_accounting(native, billing_currency, recharge_multiplier)
                    .map_err(|error| {
                        format!(
                            "procurement price requires review after exchange-rate change: {error}"
                        )
                    })?;
                if current != approved {
                    return Err(
                        "procurement price requires review after exchange-rate change".into(),
                    );
                }
            }
            _ => {
                return Err(format!(
                    "approved procurement price has an incomplete {native_field} conversion snapshot"
                ));
            }
        }
    }
    if !compared {
        return Err("approved procurement price has no conversion snapshot".into());
    }
    Ok(())
}

fn decimal_snapshot(source: &Value, field: &str) -> Result<Option<Decimal>, String> {
    let Some(value) = source.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let encoded = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    encoded
        .parse::<Decimal>()
        .map(Some)
        .map_err(|error| format!("invalid {field} in approved procurement price: {error}"))
}

fn validate_price_coverage(price: &ModelPrice, request_type: RequestType) -> Result<(), String> {
    if price.items.is_empty() {
        return Err("procurement price has no items".into());
    }
    let entirely_flat = price
        .items
        .iter()
        .all(|item| item.pricing.mode == PRICING_MODE_FLAT_FEE);
    if entirely_flat {
        return Ok(());
    }
    let codes = price
        .items
        .iter()
        .map(|item| item.item_code.as_str())
        .collect::<BTreeSet<_>>();
    let required: &[&str] = match request_type {
        RequestType::Chat | RequestType::Completion | RequestType::Compact => &[
            price_item_code::USAGE,
            price_item_code::COMPLETION,
            price_item_code::PROMPT_CACHED_TOKEN,
            price_item_code::WRITE_CACHED_TOKENS,
        ],
        RequestType::Embedding | RequestType::Rerank => &[price_item_code::USAGE],
        _ => {
            return Err("non-token request requires an explicit flat procurement price".into());
        }
    };
    let missing = required
        .iter()
        .filter(|code| !codes.contains(**code))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "procurement price is missing required items: {}",
            missing.join(", ")
        ))
    }
}

fn estimated_usage(request: &CandidateRequest) -> Usage {
    let prompt_tokens =
        u64::try_from(conduit_orchestrator::candidates::estimate_prompt_tokens(request).max(1))
            .unwrap_or(1);
    let completion_tokens = request.max_output_tokens.map(u64::from).unwrap_or(4096);
    Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
        ..Usage::default()
    }
}

fn normalize_cost_scores(candidates: &mut [ChannelModelsCandidate]) {
    let costs = candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .theoretical_cost_accounting
                .as_deref()?
                .parse::<Decimal>()
                .ok()
        })
        .collect::<Vec<_>>();
    let Some(minimum) = costs.iter().copied().min() else {
        return;
    };
    let maximum = costs.iter().copied().max().unwrap_or(minimum);
    for candidate in candidates {
        let Some(cost) = candidate
            .theoretical_cost_accounting
            .as_deref()
            .and_then(|value| value.parse::<Decimal>().ok())
        else {
            continue;
        };
        candidate.cost_efficiency_score = if maximum == minimum {
            1000
        } else {
            (((maximum - cost) / (maximum - minimum)) * Decimal::from(1000))
                .round()
                .to_i64()
                .unwrap_or(0)
                .clamp(0, 1000)
        };
    }
}

fn configuration_error(message: impl Into<String>) -> ConduitError {
    ConduitError::new(ErrorKind::Config, message)
        .with_http_status(503)
        .with_code("billing_configuration_incomplete")
        .with_safe_message("Billing configuration is incomplete")
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::channel_settings::{
        ChannelEndpoint, ChannelPolicies, ChannelSettings,
    };
    use conduit_core::objects::pricing::{ModelPriceItem, PRICING_MODE_USAGE_PER_UNIT, Pricing};
    use conduit_services::channel_service::{ChannelModelEntry, ModelSource};

    fn item(code: &str) -> ModelPriceItem {
        ModelPriceItem {
            item_code: code.into(),
            pricing: Pricing {
                mode: PRICING_MODE_USAGE_PER_UNIT.into(),
                usage_per_unit: Some(Decimal::ONE),
                ..Pricing::default()
            },
            ..ModelPriceItem::default()
        }
    }

    #[test]
    fn chat_pricing_requires_explicit_prompt_cache_items() {
        let price = ModelPrice {
            items: vec![
                item(price_item_code::USAGE),
                item(price_item_code::COMPLETION),
            ],
        };
        let error = validate_price_coverage(&price, RequestType::Chat).unwrap_err();
        assert!(error.contains(price_item_code::PROMPT_CACHED_TOKEN));
        assert!(error.contains(price_item_code::WRITE_CACHED_TOKENS));
    }

    #[test]
    fn mixed_flat_and_token_pricing_still_requires_prompt_cache_items() {
        let mut flat = item("request_fee");
        flat.pricing = Pricing {
            mode: PRICING_MODE_FLAT_FEE.into(),
            flat_fee: Some(Decimal::ONE),
            ..Pricing::default()
        };
        let price = ModelPrice {
            items: vec![
                flat,
                item(price_item_code::USAGE),
                item(price_item_code::COMPLETION),
            ],
        };

        let error = validate_price_coverage(&price, RequestType::Chat).unwrap_err();
        assert!(error.contains(price_item_code::PROMPT_CACHED_TOKEN));
        assert!(error.contains(price_item_code::WRITE_CACHED_TOKENS));
    }

    #[test]
    fn candidate_cost_only_requires_the_model_selected_for_execution() {
        let priced_model = ChannelModelEntry {
            request_model: "public-model".into(),
            actual_model: "priced-model".into(),
            source: ModelSource::Direct,
        };
        let unpriced_alternate = ChannelModelEntry {
            request_model: "public-model".into(),
            actual_model: "unpriced-alternate".into(),
            source: ModelSource::Direct,
        };
        let candidate = ChannelModelsCandidate {
            channel_id: "7".into(),
            channel_name: "channel".into(),
            ordering_weight: 0,
            priority: 0,
            models: vec![priced_model, unpriced_alternate],
            endpoint: ChannelEndpoint::default(),
            api_format: String::new(),
            channel_type: String::new(),
            policies: ChannelPolicies::default(),
            credential_key_identity: String::new(),
            tags: Vec::new(),
            base_url: None,
            active_credential: None,
            enabled_credentials: Vec::new(),
            settings: None::<ChannelSettings>,
            theoretical_cost_accounting: None,
            cost_efficiency_score: 0,
        };
        let price = ModelPrice {
            items: vec![
                item(price_item_code::USAGE),
                item(price_item_code::COMPLETION),
                item(price_item_code::PROMPT_CACHED_TOKEN),
                item(price_item_code::WRITE_CACHED_TOKENS),
            ],
        };
        let prices = BTreeMap::from([(
            (7, "priced-model".to_string()),
            StoredPrice {
                currency: AccountingSettings::default().accounting_currency,
                price: serde_json::to_value(price).unwrap(),
                approved_source: None,
                channel_billing_currency: String::new(),
                channel_recharge_multiplier: None,
            },
        )]);

        let cost = candidate_cost(
            &candidate,
            RequestType::Chat,
            &Usage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
                ..Usage::default()
            },
            &AccountingSettings::default(),
            &prices,
        )
        .unwrap();

        assert!(cost > Decimal::ZERO);
    }

    fn approved_source(version: u64, rate: Decimal) -> Value {
        let accounting_input = Decimal::from(10) / Decimal::from(2) / rate;
        serde_json::json!({
            "billingCurrency": "USD",
            "rechargeMultiplier": "2",
            "accountingCurrency": "CNY",
            "input": "10",
            "output": null,
            "cacheRead": null,
            "cacheWrite": null,
            "flat": null,
            "accountingInput": accounting_input.normalize().to_string(),
            "accountingOutput": null,
            "accountingCacheRead": null,
            "accountingCacheWrite": null,
            "accountingFlat": null,
            "accountingSettingsVersion": version,
        })
    }

    #[test]
    fn approved_conversion_is_rejected_when_its_exchange_rate_changed() {
        let source = approved_source(1, Decimal::new(14, 2));
        let settings = AccountingSettings {
            exchange_rates: vec![conduit_core::objects::money::CurrencyExchangeRate {
                currency: "USD".into(),
                quote_per_accounting_unit: Decimal::new(15, 2),
            }],
            version: 2,
            ..AccountingSettings::default()
        };

        let error = validate_approved_conversion(&source, &settings).unwrap_err();
        assert!(error.contains("requires review after exchange-rate change"));
    }

    #[test]
    fn approved_conversion_survives_an_unrelated_settings_version_change() {
        let rate = Decimal::new(14, 2);
        let source = approved_source(1, rate);
        let settings = AccountingSettings {
            credit_display_name: "Credits".into(),
            credits_per_accounting_unit: Decimal::from(20_000),
            exchange_rates: vec![conduit_core::objects::money::CurrencyExchangeRate {
                currency: "USD".into(),
                quote_per_accounting_unit: rate,
            }],
            version: 2,
            ..AccountingSettings::default()
        };

        validate_approved_conversion(&source, &settings).unwrap();
    }

    #[test]
    fn approved_conversion_is_rejected_when_channel_recharge_metadata_changes() {
        let source = approved_source(1, Decimal::new(14, 2));

        validate_channel_billing_snapshot(&source, "usd", Some(Decimal::from(2))).unwrap();
        let currency_error =
            validate_channel_billing_snapshot(&source, "EUR", Some(Decimal::from(2))).unwrap_err();
        assert!(currency_error.contains("channel billing conversion change"));
        let multiplier_error =
            validate_channel_billing_snapshot(&source, "USD", Some(Decimal::from(3))).unwrap_err();
        assert!(multiplier_error.contains("channel billing conversion change"));
    }
}

//! PostgreSQL-backed authenticated model market/catalog.
//!
//! Public models (customer-facing SKUs), upstream deployments, and routes are
//! kept as three separate identities.  In particular, two channels may expose
//! the same provider model name without either route being inferred from that
//! name: entitlement filtering is performed through the concrete route and
//! deployment IDs resolved by `wiring_project_access`.

use conduit_core::objects::apikey::{APIKeyProfile, APIKeyProfiles};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use async_graphql::ID;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::groups::GroupServices;
use conduit_admin_graphql::model_catalog as gql;
use conduit_core::objects::model::ModelCard;
use conduit_core::objects::money::{AccountingSettings, STATION_CREDIT_CODE};
use conduit_core::objects::pricing::{ModelPrice, price_item_code};
use conduit_core::objects::retail_price_plan::RetailPriceDocument;
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::SystemService;
use rust_decimal::Decimal;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};

#[derive(Clone)]
pub struct PgModelMarketAdapter {
    pool: PgPool,
    system: Arc<SystemService>,
}

impl PgModelMarketAdapter {
    pub fn new(pool: PgPool, system: Arc<SystemService>) -> Self {
        Self { pool, system }
    }

    async fn health_visible(&self) -> bool {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        self.system
            .channel_setting_or_default(&ctx)
            .await
            .extra
            .get("expose_public_channel_health")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

#[derive(FromRow)]
struct PublicModelRow {
    id: i64,
    model_id: String,
    name: String,
    developer: String,
    model_type: String,
    model_group: String,
    model_card: Json<ModelCard>,
}

#[derive(FromRow)]
struct RouteRow {
    channel_id: i64,
    channel_name: String,
    route_type: String,
}

#[derive(FromRow)]
struct HealthRow {
    total: i64,
    success: i64,
    ttft: Option<f64>,
    tps: Option<f64>,
    updated: Option<i64>,
}

async fn require_project_membership(
    pool: &PgPool,
    user_id: i64,
    project_id: i64,
) -> Result<(), gql::ModelCatalogError> {
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS( \
           SELECT 1 FROM user_projects up \
           JOIN projects p ON p.id = up.project_id \
           WHERE up.user_id = $1 AND up.project_id = $2 \
             AND p.status = 'active' AND p.deleted_at = 0 \
         )",
    )
    .bind(user_id)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map_err(query)?;
    if allowed {
        Ok(())
    } else {
        Err(gql::ModelCatalogError::ProjectAccessDenied)
    }
}

fn capabilities(card: &ModelCard) -> Vec<String> {
    let mut values = Vec::new();
    if card.reasoning.supported {
        values.push("reasoning".to_string());
    }
    if card.tool_call {
        values.push("tools".to_string());
    }
    if card.vision {
        values.push("vision".to_string());
    }
    values.extend(
        card.modalities
            .input
            .iter()
            .chain(&card.modalities.output)
            .cloned(),
    );
    values.sort();
    values.dedup();
    values
}

fn health_status(rate: Option<f64>) -> String {
    match rate {
        None => "UNKNOWN",
        Some(value) if value >= 99.0 => "OPERATIONAL",
        Some(value) if value >= 90.0 => "DEGRADED",
        Some(_) => "DISRUPTED",
    }
    .to_string()
}

fn public_route_identity(channel_id: i64) -> (ID, String) {
    let mut hasher = DefaultHasher::new();
    ("conduit-public-route-v1", channel_id).hash(&mut hasher);
    let token = format!("{:08X}", hasher.finish() as u32);
    (
        ID::from(format!("route-{token}")),
        format!("Route {}", &token[..4]),
    )
}

fn core_node_id(kind: &str, id: i64) -> ID {
    ID::from(format!("gid://conduit/{kind}/{id}"))
}

async fn channel_health(
    pool: &PgPool,
    channel_id: i64,
) -> Result<gql::MyCatalogHealth, gql::ModelCatalogError> {
    let row = sqlx::query_as::<_, HealthRow>(
        "SELECT COALESCE(SUM(total_request_count), 0)::BIGINT AS total, \
                COALESCE(SUM(success_request_count), 0)::BIGINT AS success, \
                AVG(avg_time_to_first_token_ms) AS ttft, \
                AVG(avg_tokens_per_second) AS tps, MAX(timestamp) AS updated \
         FROM channel_probes WHERE channel_id = $1 AND timestamp >= $2",
    )
    .bind(channel_id)
    .bind((Utc::now() - chrono::Duration::hours(24)).timestamp())
    .fetch_one(pool)
    .await
    .map_err(query)?;
    let rate =
        (row.total > 0).then(|| (row.success as f64 * 1000.0 / row.total as f64).round() / 10.0);
    Ok(gql::MyCatalogHealth {
        status: health_status(rate),
        success_rate: rate,
        avg_time_to_first_token_ms: row.ttft,
        avg_tokens_per_second: row.tps,
        last_updated_at: row
            .updated
            .and_then(|value| DateTime::<Utc>::from_timestamp(value, 0))
            .map(|value| value.to_rfc3339()),
    })
}

// Validate the stored wire shape before deserializing the permissive domain
// types: missing/defaulted values must never turn malformed prices into zero.
fn catalog_price_shape(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    fn fields(value: &Value, allowed: &[&str], required: &[&str]) -> bool {
        value.as_object().is_some_and(|object| {
            object.keys().all(|key| allowed.contains(&key.as_str()))
                && required.iter().all(|key| object.contains_key(*key))
        })
    }
    fn pricing(value: &Value) -> bool {
        if !fields(
            value,
            &["mode", "flatFee", "usagePerUnit", "usageTiered"],
            &["mode"],
        ) {
            return false;
        }
        let active = match value["mode"].as_str() {
            Some("flat_fee") => "flatFee",
            Some("usage_per_unit") => "usagePerUnit",
            Some("usage_tiered" | "usage_volume") => "usageTiered",
            _ => return false,
        };
        if ["flatFee", "usagePerUnit", "usageTiered"]
            .iter()
            .any(|key| *key != active && value.get(*key).is_some_and(|v| !v.is_null()))
        {
            return false;
        }
        if active != "usageTiered" {
            return value
                .get(active)
                .is_some_and(|v| v.is_string() || v.is_number());
        }
        let tiers = &value[active];
        fields(tiers, &["tiers"], &["tiers"])
            && tiers["tiers"].as_array().is_some_and(|tiers| {
                tiers.iter().all(|tier| {
                    fields(tier, &["upTo", "pricePerUnit"], &["pricePerUnit"])
                        && (tier["pricePerUnit"].is_string() || tier["pricePerUnit"].is_number())
                })
            })
    }
    fields(value, &["items"], &["items"])
        && value["items"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                fields(
                    item,
                    &["itemCode", "pricing", "promptWriteCacheVariants"],
                    &["itemCode", "pricing"],
                ) && item["itemCode"]
                    .as_str()
                    .is_some_and(|code| !code.is_empty())
                    && pricing(&item["pricing"])
                    && item.get("promptWriteCacheVariants").is_none_or(|variants| {
                        variants.as_array().is_some_and(|variants| {
                            variants.iter().all(|variant| {
                                fields(
                                    variant,
                                    &["variantCode", "pricing"],
                                    &["variantCode", "pricing"],
                                ) && variant["variantCode"]
                                    .as_str()
                                    .is_some_and(|code| !code.is_empty())
                                    && pricing(&variant["pricing"])
                            })
                        })
                    })
            })
        })
}

fn catalog_pricing(
    pricing: conduit_core::objects::pricing::Pricing,
    multiplier: Decimal,
    accounting: &AccountingSettings,
) -> Option<gql::MyCatalogPricing> {
    let convert = |amount: Decimal| {
        amount
            .checked_mul(multiplier)?
            .checked_mul(accounting.credits_per_accounting_unit)
            .map(|amount| amount.normalize().to_string())
    };
    let amount = match pricing.mode.as_str() {
        "flat_fee" => Some(convert(pricing.flat_fee?)?),
        "usage_per_unit" => Some(convert(pricing.usage_per_unit?)?),
        _ => None,
    };
    let mut above = 0;
    let tiers = pricing
        .usage_tiered
        .into_iter()
        .flat_map(|pricing| pricing.tiers)
        .map(|tier| {
            let result = gql::MyCatalogPriceTier {
                above: above.to_string(),
                up_to: tier.up_to.map(|bound| bound.to_string()),
                price_per_million: convert(tier.price_per_unit)?,
            };
            above = tier.up_to.unwrap_or(above);
            Some(result)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(gql::MyCatalogPricing {
        mode: pricing.mode,
        amount,
        tiers,
    })
}

fn retail_price(
    price: Option<serde_json::Value>,
    multiplier: Decimal,
    accounting: &AccountingSettings,
) -> gql::MyCatalogPrice {
    use gql::MyCatalogPriceStatus;
    let mut result = gql::MyCatalogPrice {
        status: MyCatalogPriceStatus::Unpriced,
        items: Vec::new(),
        branches: Vec::new(),
        source: "global".into(),
        price_book_version: None,
        currency: STATION_CREDIT_CODE.to_string(),
        display_name: accounting.credit_display_name.clone(),
        billable: false,
        input_per_million: None,
        output_per_million: None,
        cache_read_per_million: None,
        cache_write_per_million: None,
        effective_multiplier: multiplier.normalize().to_string(),
    };
    let Some(raw) = price else {
        return result;
    };
    result.status = MyCatalogPriceStatus::Unavailable;
    if !catalog_price_shape(&raw)
        || multiplier.is_sign_negative()
        || accounting.credits_per_accounting_unit <= Decimal::ZERO
    {
        return result;
    }
    let Ok(price) = serde_json::from_value::<ModelPrice>(raw) else {
        return result;
    };
    if price.items.is_empty() {
        result.status = MyCatalogPriceStatus::Unpriced;
        return result;
    }
    if crate::wiring::validate_model_price(&price).is_err() {
        return result;
    }
    let items = price
        .items
        .into_iter()
        .map(|item| {
            Some(gql::MyCatalogPriceItem {
                item_code: item.item_code,
                pricing: catalog_pricing(item.pricing, multiplier, accounting)?,
                prompt_write_cache_variants: item
                    .prompt_write_cache_variants
                    .into_iter()
                    .map(|variant| {
                        Some(gql::MyCatalogPriceVariant {
                            variant_code: variant.variant_code,
                            pricing: catalog_pricing(variant.pricing, multiplier, accounting)?,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(items) = items else {
        return result;
    };
    // Legacy token summaries are only meaningful for unique, unqualified
    // per-unit token items. Complex prices are represented by the full items.
    let simple = items.iter().all(|item| {
        item.pricing.mode == "usage_per_unit"
            && item.prompt_write_cache_variants.is_empty()
            && matches!(
                item.item_code.as_str(),
                price_item_code::USAGE
                    | price_item_code::COMPLETION
                    | price_item_code::PROMPT_CACHED_TOKEN
                    | price_item_code::WRITE_CACHED_TOKENS
            )
    }) && items
        .iter()
        .map(|item| &item.item_code)
        .collect::<BTreeSet<_>>()
        .len()
        == items.len();
    if simple {
        for item in &items {
            let amount = item.pricing.amount.clone();
            match item.item_code.as_str() {
                price_item_code::USAGE => result.input_per_million = amount,
                price_item_code::COMPLETION => result.output_per_million = amount,
                price_item_code::PROMPT_CACHED_TOKEN => result.cache_read_per_million = amount,
                price_item_code::WRITE_CACHED_TOKENS => result.cache_write_per_million = amount,
                _ => {}
            }
        }
    }
    result.items = items;
    result.status = MyCatalogPriceStatus::Priced;
    result.billable = true;
    result
}

// Only the authorized group's reachable tree is exposed. Token conditions stay
// explicit: a catalog preview cannot predict final usage or pick a cheapest leaf.
fn scoped_retail_price(
    raw: Option<serde_json::Value>,
    group: &str,
    multiplier: Decimal,
    accounting: &AccountingSettings,
    version: Option<i64>,
) -> gql::MyCatalogPrice {
    let mut result = retail_price(None, multiplier, accounting);
    result.price_book_version = version.map(|id| id.to_string());
    let Some(raw) = raw else {
        return result;
    };
    if raw.get("schema").is_none() && raw.get("groups").is_none() && raw.get("global").is_none() {
        let mut legacy = retail_price(Some(raw), multiplier, accounting);
        legacy.price_book_version = result.price_book_version;
        return legacy;
    }
    result.status = gql::MyCatalogPriceStatus::Unavailable;
    let Ok(document) = RetailPriceDocument::parse(&raw) else {
        return result;
    };
    let branch = document
        .groups
        .iter()
        .find(|branch| branch.model_group_id == group);
    let mut leaves = Vec::new();
    if let Some(branch) = branch {
        for rule in &branch.rules {
            let Ok(condition) = serde_json::to_string(&rule.condition) else {
                return result;
            };
            leaves.push(("rule", rule.label.clone(), Some(condition), &rule.price));
        }
        if let Some(base) = &branch.base {
            leaves.push(("group_base", String::new(), None, base));
        }
    }
    if branch.is_none_or(|branch| branch.base.is_none()) {
        if let Some(global) = &document.global {
            leaves.push(("global", String::new(), None, global));
        }
    }
    if leaves.is_empty() {
        result.status = gql::MyCatalogPriceStatus::Unpriced;
        return result;
    }
    let conditional = leaves.iter().any(|leaf| leaf.0 == "rule");
    for (source, label, condition, leaf) in leaves {
        let Ok(value) = serde_json::to_value(leaf) else {
            return result;
        };
        let projection = retail_price(Some(value), multiplier, accounting);
        if projection.status != gql::MyCatalogPriceStatus::Priced {
            return result;
        }
        if !conditional {
            let mut projection = projection;
            projection.source = source.into();
            projection.price_book_version = result.price_book_version;
            return projection;
        }
        result.branches.push(gql::MyCatalogPriceBranch {
            source: source.into(),
            label,
            condition,
            items: projection.items,
        });
    }
    result.status = gql::MyCatalogPriceStatus::Priced;
    result.billable = true;
    result.source = "conditional".into();
    result
}

fn aggregate_health(routes: &[gql::MyCatalogRoute]) -> gql::MyCatalogHealth {
    // Route health has already been loaded.  Keeping this helper free of any
    // channel/deployment lookup prevents a name-based rejoin from being added
    // accidentally in the future.
    let rates = routes
        .iter()
        .filter_map(|route| route.health.as_ref()?.success_rate)
        .collect::<Vec<_>>();
    let rate = (!rates.is_empty()).then(|| rates.iter().sum::<f64>() / rates.len() as f64);
    let ttfts = routes
        .iter()
        .filter_map(|route| route.health.as_ref()?.avg_time_to_first_token_ms)
        .collect::<Vec<_>>();
    let tps = routes
        .iter()
        .filter_map(|route| route.health.as_ref()?.avg_tokens_per_second)
        .collect::<Vec<_>>();
    let last_updated_at = routes
        .iter()
        .filter_map(|route| route.health.as_ref()?.last_updated_at.as_ref())
        .max()
        .cloned();
    gql::MyCatalogHealth {
        status: health_status(rate),
        success_rate: rate,
        avg_time_to_first_token_ms: (!ttfts.is_empty())
            .then(|| ttfts.iter().sum::<f64>() / ttfts.len() as f64),
        avg_tokens_per_second: (!tps.is_empty())
            .then(|| tps.iter().sum::<f64>() / tps.len() as f64),
        last_updated_at,
    }
}

#[async_trait::async_trait]
impl gql::ModelCatalogServices for PgModelMarketAdapter {
    async fn my_model_catalog(
        &self,
        user_id: i64,
        project_id: i64,
        model_group_id: Option<String>,
        api_key_id: Option<String>,
    ) -> Result<gql::MyModelCatalog, gql::ModelCatalogError> {
        require_project_membership(&self.pool, user_id, project_id).await?;
        if model_group_id.is_some() && api_key_id.is_some() {
            return Err(gql::ModelCatalogError::InvalidPriceScope);
        }
        let groups = crate::wiring_postgres_groups::PgGroupAdapter::new(self.pool.clone())
            .assignable(user_id, project_id)
            .await
            .map_err(gql::ModelCatalogError::Query)?;
        let mut scopes = groups
            .iter()
            .map(|group| gql::MyCatalogPriceScope {
                id: ID(format!("group:{}", group.id.as_str())),
                label: group.name.clone(),
                model_group_id: group.id.clone(),
                model_group_revision: group.revision,
                api_key_id: None,
            })
            .collect::<Vec<_>>();
        let keys: Vec<(i64, String, String, Json<serde_json::Value>)> = sqlx::query_as("SELECT k.id,k.name,g.model_group_id,k.profiles FROM api_keys k JOIN api_key_model_groups g ON g.api_key_id=k.id WHERE k.user_id=$1 AND k.project_id=$2 AND k.status='enabled' AND k.deleted_at=0 AND k.\"type\"='user' ORDER BY k.name,k.id")
            .bind(user_id).bind(project_id).fetch_all(&self.pool).await.map_err(query)?;
        let mut key_profiles = BTreeMap::<i64, Option<APIKeyProfile>>::new();
        for (id, name, group_id, profiles) in keys {
            let Ok(profiles) = serde_json::from_value::<APIKeyProfiles>(profiles.0) else {
                continue;
            };
            if conduit_services::apikey_service::validate_all_profiles(&profiles).is_err() {
                continue;
            }
            let active = if profiles.active_profile.is_empty() {
                None
            } else {
                let Some(active) = profiles
                    .profiles
                    .iter()
                    .find(|profile| profile.name == profiles.active_profile)
                else {
                    continue;
                };
                let now = Utc::now();
                if active.valid_from.is_some_and(|at| now < at)
                    || active.valid_until.is_some_and(|at| now >= at)
                {
                    continue;
                }
                Some(active.clone())
            };
            key_profiles.insert(id, active);
            if let Some(group) = groups.iter().find(|group| group.id.as_str() == group_id) {
                scopes.push(gql::MyCatalogPriceScope {
                    id: ID(format!("key:{id}")),
                    label: name,
                    model_group_id: group.id.clone(),
                    model_group_revision: group.revision,
                    api_key_id: Some(core_node_id("APIKey", id)),
                });
            }
        }
        let selected_scope = if let Some(group) = model_group_id {
            Some(
                scopes
                    .iter()
                    .find(|scope| {
                        scope.api_key_id.is_none() && scope.model_group_id.as_str() == group
                    })
                    .ok_or(gql::ModelCatalogError::InvalidPriceScope)?
                    .clone(),
            )
        } else if let Some(key) = api_key_id {
            let key_id = key
                .strip_prefix("gid://conduit/APIKey/")
                .unwrap_or(&key)
                .parse::<i64>()
                .map_err(|_| gql::ModelCatalogError::InvalidPriceScope)?;
            let trusted = crate::wiring_postgres_groups::trusted_key_group(
                &self.pool, key_id, user_id, project_id,
            )
            .await
            .map_err(query)?
            .ok_or(gql::ModelCatalogError::InvalidPriceScope)?;
            Some(
                scopes
                    .iter()
                    .find(|scope| {
                        scope.id.as_str() == format!("key:{key_id}")
                            && scope.model_group_id.as_str() == trusted.0
                            && scope.model_group_revision == trusted.1
                    })
                    .ok_or(gql::ModelCatalogError::InvalidPriceScope)?
                    .clone(),
            )
        } else {
            None
        };
        let Some(scope) = selected_scope else {
            return Ok(gql::MyModelCatalog {
                models: vec![],
                health_visible: self.health_visible().await,
                price_scopes: scopes,
                selected_price_scope: None,
            });
        };
        let key_profile = scope
            .api_key_id
            .as_ref()
            .and_then(|id| id.as_str().strip_prefix("gid://conduit/APIKey/"))
            .and_then(|id| id.parse::<i64>().ok())
            .and_then(|id| key_profiles.get(&id))
            .and_then(Option::as_ref);
        let group = groups
            .iter()
            .find(|group| group.id == scope.model_group_id)
            .ok_or(gql::ModelCatalogError::InvalidPriceScope)?;
        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &self.pool, project_id,
        )
        .await?;
        let price_multiplier =
            crate::wiring_project_access::resolve_effective_project_price_multiplier_postgres(
                &self.pool,
                project_id,
                Utc::now(),
            )
            .await?;
        let health_visible = self.health_visible().await;
        let accounting = crate::usage_charge_settler_postgres::load_accounting_settings(&self.pool)
            .await
            .map_err(gql::ModelCatalogError::Query)?;
        let now = Utc::now();
        let price_version: Option<(i64, String)> = sqlx::query_as(
            "SELECT v.id, b.currency FROM price_books b \
             JOIN price_book_versions v ON v.price_book_id = b.id \
             WHERE b.is_default = TRUE AND b.status = 'enabled' \
               AND v.status = 'published' \
               AND (v.effective_start_at IS NULL OR v.effective_start_at <= $1) \
               AND (v.effective_end_at IS NULL OR v.effective_end_at > $1) \
             ORDER BY v.version DESC, v.id DESC LIMIT 1",
        )
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(query)?;
        let models = sqlx::query_as::<_, PublicModelRow>(
            "SELECT id, model_id, name, developer, \"type\" AS model_type, \
                    \"group\" AS model_group, model_card \
             FROM models WHERE status = 'enabled' AND deleted_at = 0 \
             ORDER BY lower(name), id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(query)?;

        let mut output = Vec::new();
        for model in models {
            if !group
                .model_ids
                .iter()
                .any(|id| id.as_str() == model.model_id)
            {
                continue;
            }
            if key_profile.is_some_and(|profile| {
                !profile.model_ids.is_empty() && !profile.model_ids.contains(&model.model_id)
            }) {
                continue;
            }
            let allowed_channels = access
                .channels_for_model(&model.model_id)
                .into_iter()
                .filter(|id| {
                    key_profile.is_none_or(|profile| {
                        profile.channel_ids.is_empty() || profile.channel_ids.contains(id)
                    })
                })
                .collect::<Vec<_>>();
            if allowed_channels.is_empty() {
                continue;
            }
            let route_rows = sqlx::query_as::<_, RouteRow>(
                "SELECT d.channel_id, c.name AS channel_name, c.\"type\" AS route_type \
                 FROM model_routes r \
                 JOIN upstream_model_deployments d ON d.id = r.deployment_id \
                 JOIN channels c ON c.id = d.channel_id \
                 WHERE r.public_model_id = $1 AND r.status = 'enabled' \
                   AND d.status = 'enabled' AND c.status = 'enabled' \
                   AND c.deleted_at = 0 ORDER BY r.id",
            )
            .bind(model.id)
            .fetch_all(&self.pool)
            .await
            .map_err(query)?;
            let mut routes = Vec::new();
            let mut seen_channels = BTreeSet::new();
            for route in route_rows {
                if !allowed_channels.contains(&route.channel_id)
                    || !seen_channels.insert(route.channel_id)
                {
                    continue;
                }
                let health = if health_visible {
                    Some(channel_health(&self.pool, route.channel_id).await?)
                } else {
                    None
                };
                let (id, label) = public_route_identity(route.channel_id);
                routes.push(gql::MyCatalogRoute {
                    id,
                    channel_id: core_node_id("Channel", route.channel_id),
                    channel_name: route.channel_name,
                    label,
                    route_type: route.route_type,
                    health,
                });
            }
            if routes.is_empty() {
                continue;
            }
            let health = if health_visible {
                Some(aggregate_health(&routes))
            } else {
                None
            };
            let (version_id, currency) = price_version
                .as_ref()
                .map(|(id, currency)| (Some(*id), currency.clone()))
                .unwrap_or((None, accounting.accounting_currency.clone()));
            if !currency.eq_ignore_ascii_case(&accounting.accounting_currency) {
                return Err(gql::ModelCatalogError::Query(format!(
                    "published retail price currency {currency} does not match accounting currency {}",
                    accounting.accounting_currency
                )));
            }
            let price = if let Some(version_id) = version_id {
                sqlx::query_scalar::<_, Json<serde_json::Value>>(
                    "SELECT price FROM price_book_items \
                     WHERE price_book_version_id = $1 AND public_model_id = $2",
                )
                .bind(version_id)
                .bind(model.id)
                .fetch_optional(&self.pool)
                .await
                .map_err(query)?
                .map(|value| value.0)
            } else {
                None
            };
            let card = model.model_card.0;
            output.push(gql::MyCatalogModel {
                id: core_node_id("Model", model.id),
                model_id: model.model_id,
                name: model.name,
                group: model.model_group,
                developer: model.developer,
                model_type: model.model_type,
                capabilities: capabilities(&card),
                context_limit: (card.limit.context > 0).then_some(card.limit.context),
                output_limit: (card.limit.output > 0).then_some(card.limit.output),
                price: scoped_retail_price(
                    price,
                    scope.model_group_id.as_str(),
                    price_multiplier,
                    &accounting,
                    version_id,
                ),
                routes,
                health,
            });
        }
        Ok(gql::MyModelCatalog {
            models: output,
            health_visible,
            price_scopes: scopes,
            selected_price_scope: Some(scope.id),
        })
    }
}

fn query(error: sqlx::Error) -> gql::ModelCatalogError {
    gql::ModelCatalogError::Query(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::{Cache, NoopCache};
    use conduit_core::objects::pricing::{ModelPriceItem, PRICING_MODE_USAGE_PER_UNIT, Pricing};

    type TestError = Box<dyn std::error::Error>;

    fn project_price(value: Option<serde_json::Value>) -> gql::MyCatalogPrice {
        retail_price(value, Decimal::new(15, 1), &AccountingSettings::default())
    }

    #[test]
    fn retail_projection_distinguishes_zero_missing_and_malformed_prices() {
        use gql::MyCatalogPriceStatus::*;
        use serde_json::json;
        assert_eq!(project_price(None).status, Unpriced);
        assert_eq!(project_price(Some(json!({"items": []}))).status, Unpriced);
        let zero = project_price(Some(json!({"items": [{"itemCode":"prompt_tokens",
            "pricing":{"mode":"usage_per_unit","usagePerUnit":"0"}}]})));
        assert_eq!(zero.status, Priced);
        assert!(zero.billable);
        assert_eq!(zero.input_per_million.as_deref(), Some("0"));
        for value in [
            json!(null),
            json!({}),
            json!({"items":null}),
            json!({"items":[{"itemCode":"request","pricing":{"mode":"future","flatFee":"0"}}]}),
            json!({"items":[{"itemCode":"request","pricing":{"mode":"flat_fee"}}]}),
            json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"usage_tiered","usageTiered":{"tiers":[{}]}}}]}),
            json!({"items":[{"itemCode":"prompt_tokens","unit":1000,"pricing":{"mode":"usage_per_unit","usagePerUnit":"1"}}]}),
            json!({"items":[{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"-1"}}]}),
            json!({"items":[{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"1","usagePerUnit":"2"}}]}),
        ] {
            let price = project_price(Some(value.clone()));
            assert_eq!(price.status, Unavailable, "{value}");
            assert!(!price.billable);
            assert!(price.items.is_empty());
        }
    }

    #[test]
    fn retail_projection_preserves_mixed_tiers_variants_order_and_multiplier() {
        use serde_json::json;
        let raw = json!({"items":[
            {"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"0"}},
            {"itemCode":"prompt_tokens","pricing":{"mode":"usage_tiered","usageTiered":{"tiers":[
                {"upTo":9007199254740993_i64,"pricePerUnit":"2"},{"pricePerUnit":"3"}]}}},
            {"itemCode":"completion_tokens","pricing":{"mode":"usage_volume","usageTiered":{"tiers":[
                {"upTo":1000,"pricePerUnit":"4"},{"pricePerUnit":"5"}]}}},
            {"itemCode":"prompt_write_cached_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"6"},
             "promptWriteCacheVariants":[{"variantCode":"one_hour","pricing":{"mode":"flat_fee","flatFee":"7"}},
               {"variantCode":"five_min","pricing":{"mode":"usage_per_unit","usagePerUnit":"0"}}]},
            {"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"0.123456789012"}}
        ]});
        let mut accounting = AccountingSettings::default();
        accounting.credits_per_accounting_unit = Decimal::from(10);
        let price = retail_price(Some(raw), Decimal::new(15, 1), &accounting);
        assert_eq!(price.status, gql::MyCatalogPriceStatus::Priced);
        assert!(price.billable);
        assert_eq!(price.items.len(), 5);
        assert_eq!(price.items[0].pricing.amount.as_deref(), Some("0"));
        assert_eq!(
            price.items[1].pricing.tiers[0].up_to.as_deref(),
            Some("9007199254740993")
        );
        assert_eq!(price.items[1].pricing.tiers[1].above, "9007199254740993");
        assert!(price.items[1].pricing.tiers[1].up_to.is_none());
        assert_eq!(price.items[1].pricing.tiers[0].price_per_million, "30");
        assert_eq!(price.items[2].pricing.mode, "usage_volume");
        assert_eq!(price.items[2].pricing.tiers[1].price_per_million, "75");
        assert_eq!(
            price.items[3].prompt_write_cache_variants[0].variant_code,
            "one_hour"
        );
        assert_eq!(
            price.items[3].prompt_write_cache_variants[0]
                .pricing
                .amount
                .as_deref(),
            Some("105")
        );
        assert_eq!(
            price.items[3].prompt_write_cache_variants[1]
                .pricing
                .amount
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            price.items[4].pricing.amount.as_deref(),
            Some("1.85185183518")
        );
        assert!(price.input_per_million.is_none());
        assert!(price.cache_write_per_million.is_none());
    }

    #[test]
    fn retail_projection_rejects_invalid_tier_boundaries_and_conversion_overflow() {
        use serde_json::json;
        for tiers in [
            json!([]),
            json!([{"upTo":10,"pricePerUnit":"1"}]),
            json!([{"upTo":10,"pricePerUnit":"1"},{"upTo":5,"pricePerUnit":"2"},{"pricePerUnit":"3"}]),
            json!([{"pricePerUnit":"-1"}]),
        ] {
            let price = project_price(Some(json!({"items":[{"itemCode":"prompt_tokens",
                "pricing":{"mode":"usage_volume","usageTiered":{"tiers":tiers}}}]})));
            assert_eq!(price.status, gql::MyCatalogPriceStatus::Unavailable);
        }
        let price = project_price(Some(json!({"items":[{"itemCode":"request",
            "pricing":{"mode":"flat_fee","flatFee":Decimal::MAX.to_string()}}]})));
        assert_eq!(price.status, gql::MyCatalogPriceStatus::Unavailable);
    }

    #[test]
    fn scoped_projection_preserves_order_conditions_zero_and_fallback_without_guessing_usage() {
        use serde_json::json;
        let group = "00000000-0000-4000-8000-000000000001";
        let leaf = |value: &str| json!({"items":[{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":value}}]});
        let raw = json!({"schema":"group-conditional-price-v1","global":leaf("9"),"groups":[{"modelGroupID":group,"base":null,"rules":[{"id":"first","label":"Zero minute","condition":"minute >= 30 && total_tokens < 1000","price":leaf("0")},{"id":"second","label":"Tokens","condition":{"op":"compare","fact":"prompt_tokens","cmp":"ge","value":1000},"price":leaf("2")}]}]});
        let price = scoped_retail_price(
            Some(raw.clone()),
            group,
            Decimal::new(15, 1),
            &AccountingSettings::default(),
            Some(123),
        );
        assert_eq!(price.status, gql::MyCatalogPriceStatus::Priced);
        assert!(price.items.is_empty());
        assert!(price.input_per_million.is_none());
        assert_eq!(price.branches.len(), 3);
        assert_eq!(price.branches[0].label, "Zero minute");
        assert_eq!(
            price.branches[0].items[0].pricing.amount.as_deref(),
            Some("0")
        );
        assert!(
            price.branches[0]
                .condition
                .as_ref()
                .unwrap()
                .contains("minute")
        );
        assert_eq!(price.branches[2].source, "global");
        assert_eq!(price.price_book_version.as_deref(), Some("123"));
        let global = scoped_retail_price(
            Some(raw.clone()),
            "00000000-0000-4000-8000-000000000002",
            Decimal::ONE,
            &AccountingSettings::default(),
            None,
        );
        assert!(global.branches.is_empty());
        assert_eq!(global.source, "global");
        let mut invalid = raw;
        invalid["groups"][0]["rules"][0]["condition"] = json!("request_header == 1");
        assert_eq!(
            scoped_retail_price(
                Some(invalid),
                group,
                Decimal::ONE,
                &AccountingSettings::default(),
                None
            )
            .status,
            gql::MyCatalogPriceStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn live_postgres_catalog_keeps_same_named_deployments_route_and_price_isolated()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let isolated = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = isolated.pool.clone();
        let result: Result<(), TestError> = async {
        crate::wiring_model_catalog::ensure_upstream_model_deployments_postgres(&pool).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let user_id: i64 =
            sqlx::query_scalar("INSERT INTO users(email,password) VALUES($1,'test') RETURNING id")
                .bind(format!("pg-market-{suffix}@example.test"))
                .fetch_one(&pool)
                .await?;
        let project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects(name,description,status) \
             VALUES($1,'','active') RETURNING id",
        )
        .bind(format!("pg-market-project-{suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) \
             VALUES($1,$2,TRUE,'[]'::jsonb)",
        )
        .bind(user_id)
        .bind(project_id)
        .execute(&pool)
        .await?;

        let channel_one: i64 = sqlx::query_scalar(
            "INSERT INTO channels(\"type\",name,status,credentials,supported_models,default_test_model) \
             VALUES('openai',$1,'enabled','{}'::jsonb,'[\"same-upstream\"]'::jsonb,'same-upstream') RETURNING id",
        )
        .bind(format!("PG Market C1 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let channel_two: i64 = sqlx::query_scalar(
            "INSERT INTO channels(\"type\",name,status,credentials,supported_models,default_test_model) \
             VALUES('openai',$1,'enabled','{}'::jsonb,'[\"same-upstream\"]'::jsonb,'same-upstream') RETURNING id",
        )
        .bind(format!("PG Market C2 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let deployment_one: i64 = sqlx::query_scalar(
            "INSERT INTO upstream_model_deployments \
             (channel_id,upstream_model_id,internal_name,variant,status,source) \
             VALUES($1,'same-upstream',$2,'','enabled','test') \
             ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE \
               SET status='enabled' RETURNING id",
        )
        .bind(channel_one)
        .bind(format!("C1 / same-upstream / {suffix}"))
        .fetch_one(&pool)
        .await?;
        let deployment_two: i64 = sqlx::query_scalar(
            "INSERT INTO upstream_model_deployments \
             (channel_id,upstream_model_id,internal_name,variant,status,source) \
             VALUES($1,'same-upstream',$2,'','enabled','test') \
             ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE \
               SET status='enabled' RETURNING id",
        )
        .bind(channel_two)
        .bind(format!("C2 / same-upstream / {suffix}"))
        .fetch_one(&pool)
        .await?;
        assert_ne!(deployment_one, deployment_two);

        let public_key = format!("pg-public-{suffix}");
        let public_model_id: i64 = sqlx::query_scalar(
            "INSERT INTO models \
             (developer,model_id,\"type\",name,icon,\"group\",model_card,settings,status) \
             VALUES('test',$1,'chat',$2,'','test',$3,$4,'enabled') RETURNING id",
        )
        .bind(&public_key)
        .bind(format!("PG Public {suffix}"))
        .bind(Json(ModelCard::default()))
        .bind(Json(serde_json::json!({})))
        .fetch_one(&pool)
        .await?;
        let route_one: i64 = sqlx::query_scalar(
            "INSERT INTO model_routes(public_model_id,deployment_id,status) \
             VALUES($1,$2,'enabled') RETURNING id",
        )
        .bind(public_model_id)
        .bind(deployment_one)
        .fetch_one(&pool)
        .await?;
        let route_two: i64 = sqlx::query_scalar(
            "INSERT INTO model_routes(public_model_id,deployment_id,status) \
             VALUES($1,$2,'enabled') RETURNING id",
        )
        .bind(public_model_id)
        .bind(deployment_two)
        .fetch_one(&pool)
        .await?;

        let plan_id: i64 = sqlx::query_scalar(
            "INSERT INTO access_plans(name,status,is_default,created_at,updated_at) \
             VALUES($1,'enabled',FALSE,now(),now()) RETURNING id",
        )
        .bind(format!("pg-market-plan-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let access_version_id: i64 = sqlx::query_scalar(
            "INSERT INTO access_plan_versions \
             (access_plan_id,version,status,reference_id,created_at,updated_at) \
             VALUES($1,1,'published',$2,now(),now()) RETURNING id",
        )
        .bind(plan_id)
        .bind(format!("pg-market-plan-version-{suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) \
             VALUES($1,$2,now())",
        )
        .bind(access_version_id)
        .bind(public_model_id)
        .execute(&pool)
        .await?;
        // Only C2's concrete route is granted.  C1 has the exact same provider
        // model name, so a name-based implementation would leak it here.
        sqlx::query(
            "INSERT INTO access_plan_route_items \
             (access_plan_version_id,model_route_id,created_at) VALUES($1,$2,now())",
        )
        .bind(access_version_id)
        .bind(route_two)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_access_grants \
             (project_id,access_plan_version_id,source_type,source_id,status,created_at,updated_at) \
             VALUES($1,$2,'test',$3,'active',now(),now())",
        )
        .bind(project_id)
        .bind(access_version_id)
        .bind(format!("pg-market-grant-{suffix}"))
        .execute(&pool)
        .await?;

        let tier_id: i64 = sqlx::query_scalar(
            "INSERT INTO price_tiers \
             (name,multiplier_ppm,status,is_default,created_at,updated_at) \
             VALUES($1,1500000,'enabled',FALSE,now(),now()) RETURNING id",
        )
        .bind(format!("pg-market-tier-{suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_commercial_profiles \
             (project_id,account_type,base_price_tier_id,billing_currency,status,created_at,updated_at) \
             VALUES($1,'personal',$2,'STATION_CREDIT','active',now(),now())",
        )
        .bind(project_id)
        .bind(tier_id)
        .execute(&pool)
        .await?;
        let accounting = crate::usage_charge_settler_postgres::load_accounting_settings(&pool)
            .await
            .map_err(std::io::Error::other)?;
        let price_book_id: i64 = sqlx::query_scalar(
            "INSERT INTO price_books(name,currency,status,is_default) \
             VALUES($1,$2,'enabled',TRUE) RETURNING id",
        )
        .bind(format!("pg-market-book-{suffix}"))
        .bind(&accounting.accounting_currency)
        .fetch_one(&pool)
        .await?;
        let price_version_id: i64 = sqlx::query_scalar(
            "INSERT INTO price_book_versions \
             (price_book_id,version,status,reference_id,effective_start_at) \
             VALUES($1,$2,'published',$3,now() - interval '1 minute') RETURNING id",
        )
        .bind(price_book_id)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(format!("pg-market-price-version-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let price = ModelPrice {
            items: vec![
                ModelPriceItem {
                    item_code: price_item_code::USAGE.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(2, 0)),
                        ..Pricing::default()
                    },
                    ..ModelPriceItem::default()
                },
                ModelPriceItem {
                    item_code: price_item_code::COMPLETION.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(4, 0)),
                        ..Pricing::default()
                    },
                    ..ModelPriceItem::default()
                },
                ModelPriceItem {
                    item_code: price_item_code::PROMPT_CACHED_TOKEN.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::ONE),
                        ..Pricing::default()
                    },
                    ..ModelPriceItem::default()
                },
                ModelPriceItem {
                    item_code: price_item_code::WRITE_CACHED_TOKENS.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::from(3)),
                        ..Pricing::default()
                    },
                    ..ModelPriceItem::default()
                },
            ],
        };
        sqlx::query(
            "INSERT INTO price_book_items \
             (price_book_version_id,public_model_id,price) VALUES($1,$2,$3)",
        )
        .bind(price_version_id)
        .bind(public_model_id)
        .bind(Json(price))
        .execute(&pool)
        .await?;

        use conduit_admin_graphql::groups::{GroupStatus, SaveModelGroupInput, SaveUserGroupInput};
        let group_adapter = crate::wiring_postgres_groups::PgGroupAdapter::new(pool.clone());
        let market_group = group_adapter
            .save_model_group(SaveModelGroupInput {
                id: None,
                name: format!("Market scope {suffix}"),
                description: None,
                status: GroupStatus::Enabled,
                model_ids: vec![ID(public_key.clone())],
            })
            .await?;
        let _market_users = group_adapter
            .save_user_group(SaveUserGroupInput {
                id: None,
                name: format!("Market users {suffix}"),
                description: None,
                status: GroupStatus::Enabled,
                member_user_ids: vec![core_node_id("User", user_id)],
                model_group_ids: vec![market_group.id.clone()],
                recharge_multiplier_ppm: None,
                expected_recharge_revision: None,
            })
            .await?;
        let cache: Arc<dyn Cache> = Arc::new(NoopCache::new());
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
            cache,
        ));
        let adapter = PgModelMarketAdapter::new(pool.clone(), system);
        assert!(matches!(
            gql::ModelCatalogServices::my_model_catalog(
                &adapter,
                user_id + 1_000_000,
                project_id,
                None,
                None
            )
            .await,
            Err(gql::ModelCatalogError::ProjectAccessDenied)
        ));
        let unselected =
            gql::ModelCatalogServices::my_model_catalog(&adapter, user_id, project_id, None, None)
                .await?;
        assert!(unselected.models.is_empty());
        assert_eq!(unselected.price_scopes.len(), 1);
        assert!(matches!(
            gql::ModelCatalogServices::my_model_catalog(
                &adapter,
                user_id,
                project_id,
                Some(uuid::Uuid::new_v4().to_string()),
                None
            )
            .await,
            Err(gql::ModelCatalogError::InvalidPriceScope)
        ));
        assert!(matches!(
            gql::ModelCatalogServices::my_model_catalog(
                &adapter,
                user_id,
                project_id,
                None,
                Some("999999999".into())
            )
            .await,
            Err(gql::ModelCatalogError::InvalidPriceScope)
        ));
        let catalog = gql::ModelCatalogServices::my_model_catalog(
            &adapter,
            user_id,
            project_id,
            Some(market_group.id.to_string()),
            None,
        )
        .await?;
        assert_eq!(catalog.models.len(), 1);
        let model = &catalog.models[0];
        assert_eq!(model.model_id, public_key);
        assert_eq!(model.routes.len(), 1);
        assert_eq!(
            model.routes[0].channel_id.as_str(),
            format!("gid://conduit/Channel/{channel_two}")
        );
        assert_ne!(
            model.routes[0].channel_id.as_str(),
            format!("gid://conduit/Channel/{channel_one}")
        );
        assert_eq!(model.price.effective_multiplier, "1.5");
        assert_eq!(model.price.currency, STATION_CREDIT_CODE);
        assert_eq!(model.price.display_name, accounting.credit_display_name);
        assert_eq!(
            model.price.input_per_million,
            Some(
                (Decimal::from(3) * accounting.credits_per_accounting_unit)
                    .normalize()
                    .to_string()
            )
        );
        assert_eq!(
            model.price.output_per_million,
            Some(
                (Decimal::from(6) * accounting.credits_per_accounting_unit)
                    .normalize()
                    .to_string()
            )
        );
        assert_eq!(
            model.price.cache_read_per_million,
            Some(
                (Decimal::new(15, 1) * accounting.credits_per_accounting_unit)
                    .normalize()
                    .to_string()
            )
        );
        assert_eq!(
            model.price.cache_write_per_million,
            Some(
                (Decimal::new(45, 1) * accounting.credits_per_accounting_unit)
                    .normalize()
                    .to_string()
            )
        );

        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &pool, project_id,
        )
        .await?;
        assert_eq!(
            access.channels_for_model(&public_key),
            BTreeSet::from([channel_two])
        );
        assert_eq!(
            access
                .upstream_models_for_model(&public_key)
                .get(&channel_two)
                .map(String::as_str),
            Some("same-upstream")
        );

        use conduit_db::repo::{ApiKeyRepo, api_key_repo::CreateApiKeyInput};
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let key = conduit_db::PgApiKeyRepo::new(pool.clone()).create_api_key(&ctx, CreateApiKeyInput {
            id: String::new(), user_id: Some(user_id.to_string()), project_id: project_id.to_string(), name: "Market owner fixture".into(), key: format!("market-fixture-{suffix}"), key_type: "user".into(), scopes: vec![], profiles: None, created_at: Utc::now().to_rfc3339(), model_group_id: Some(market_group.id.to_string()),
        }).await?;
        let key_catalog = gql::ModelCatalogServices::my_model_catalog(&adapter,user_id,project_id,None,Some(key.id.clone())).await?;
        assert_eq!(key_catalog.models.len(),1);
        assert_eq!(key_catalog.models[0].price.input_per_million,model.price.input_per_million);
        let key_id:i64=key.id.parse()?;
        let profiles=serde_json::json!({"activeProfile":"restricted","profiles":[{"name":"restricted","channelIDs":[channel_one],"modelIDs":[public_key]}]});
        sqlx::query("UPDATE api_keys SET profiles=$1 WHERE id=$2").bind(Json(profiles)).bind(key_id).execute(&pool).await?;
        assert!(gql::ModelCatalogServices::my_model_catalog(&adapter,user_id,project_id,None,Some(key.id.clone())).await?.models.is_empty());
        sqlx::query("UPDATE api_keys SET profiles=$1 WHERE id=$2").bind(Json(serde_json::json!({"activeProfile":"missing","profiles":[]}))).bind(key_id).execute(&pool).await?;
        assert!(matches!(gql::ModelCatalogServices::my_model_catalog(&adapter,user_id,project_id,None,Some(key.id)).await,Err(gql::ModelCatalogError::InvalidPriceScope)));
        Ok(())
        }.await;
        isolated.cleanup().await?;
        result
    }
}

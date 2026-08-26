//! PostgreSQL-backed authenticated model market/catalog.
//!
//! Public models (customer-facing SKUs), upstream deployments, and routes are
//! kept as three separate identities.  In particular, two channels may expose
//! the same provider model name without either route being inferred from that
//! name: entitlement filtering is performed through the concrete route and
//! deployment IDs resolved by `wiring_project_access`.

use std::collections::BTreeSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use async_graphql::ID;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::model_catalog as gql;
use conduit_core::objects::model::ModelCard;
use conduit_core::objects::money::{AccountingSettings, STATION_CREDIT_CODE};
use conduit_core::objects::pricing::{ModelPrice, price_item_code};
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

fn retail_price(
    price: Option<ModelPrice>,
    multiplier: Decimal,
    accounting: &AccountingSettings,
) -> gql::MyCatalogPrice {
    let mut input = None;
    let mut output = None;
    let mut cache_read = None;
    let mut cache_write = None;
    if let Some(price) = price {
        for item in price.items {
            let amount = item
                .pricing
                .usage_per_unit
                .map(|value| value * multiplier * accounting.credits_per_accounting_unit);
            match item.item_code.as_str() {
                price_item_code::USAGE => input = amount.map(|value| value.normalize().to_string()),
                price_item_code::COMPLETION => {
                    output = amount.map(|value| value.normalize().to_string())
                }
                price_item_code::PROMPT_CACHED_TOKEN => {
                    cache_read = amount.map(|value| value.normalize().to_string())
                }
                price_item_code::WRITE_CACHED_TOKENS => {
                    cache_write = amount.map(|value| value.normalize().to_string())
                }
                _ => {}
            }
        }
    }
    gql::MyCatalogPrice {
        currency: STATION_CREDIT_CODE.to_string(),
        display_name: accounting.credit_display_name.clone(),
        billable: input.is_some()
            || output.is_some()
            || cache_read.is_some()
            || cache_write.is_some(),
        input_per_million: input,
        output_per_million: output,
        cache_read_per_million: cache_read,
        cache_write_per_million: cache_write,
        effective_multiplier: multiplier.normalize().to_string(),
    }
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
    ) -> Result<gql::MyModelCatalog, gql::ModelCatalogError> {
        require_project_membership(&self.pool, user_id, project_id).await?;
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
            let allowed_channels = access.channels_for_model(&model.model_id);
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
                sqlx::query_scalar::<_, Json<ModelPrice>>(
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
                price: retail_price(price, price_multiplier, &accounting),
                routes,
                health,
            });
        }
        Ok(gql::MyModelCatalog {
            models: output,
            health_visible,
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

    #[tokio::test]
    async fn live_postgres_catalog_keeps_same_named_deployments_route_and_price_isolated()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
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

        let cache: Arc<dyn Cache> = Arc::new(NoopCache::new());
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
            cache,
        ));
        let adapter = PgModelMarketAdapter::new(pool.clone(), system);
        assert!(matches!(
            gql::ModelCatalogServices::my_model_catalog(&adapter, user_id + 1_000_000, project_id)
                .await,
            Err(gql::ModelCatalogError::ProjectAccessDenied)
        ));
        let catalog =
            gql::ModelCatalogServices::my_model_catalog(&adapter, user_id, project_id).await?;
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

        sqlx::query("DELETE FROM price_book_items WHERE price_book_version_id=$1")
            .bind(price_version_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM price_book_versions WHERE id=$1")
            .bind(price_version_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM price_books WHERE id=$1")
            .bind(price_book_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM project_commercial_profiles WHERE project_id=$1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM price_tiers WHERE id=$1")
            .bind(tier_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM project_access_grants WHERE project_id=$1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM access_plan_route_items WHERE access_plan_version_id=$1")
            .bind(access_version_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM access_plan_items WHERE access_plan_version_id=$1")
            .bind(access_version_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM access_plan_versions WHERE id=$1")
            .bind(access_version_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM access_plans WHERE id=$1")
            .bind(plan_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM model_routes WHERE id IN ($1,$2)")
            .bind(route_one)
            .bind(route_two)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM models WHERE id=$1")
            .bind(public_model_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM upstream_model_deployments WHERE channel_id IN ($1,$2)")
            .bind(channel_one)
            .bind(channel_two)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM channels WHERE id IN ($1,$2)")
            .bind(channel_one)
            .bind(channel_two)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_projects WHERE user_id=$1 AND project_id=$2")
            .bind(user_id)
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }
}

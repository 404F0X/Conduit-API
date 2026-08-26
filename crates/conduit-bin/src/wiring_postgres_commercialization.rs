//! PostgreSQL commercialization boundary.
//!
//! Public model SKUs, upstream deployments and routes are distinct entities:
//! equal provider model names on two channels remain two deployments. Retail
//! prices are immutable published snapshots. Editable retail prices live in
//! the unified change-set store until an approval transaction publishes them.

use std::collections::BTreeSet;

use async_graphql::{ID, Json};
use chrono::{DateTime, Utc};
use conduit_admin_graphql::commercialization as gql;
use conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE;
use conduit_core::objects::pricing::ModelPrice;
use conduit_db::row::ModelRow;
use sqlx::{PgPool, Postgres, Row, Transaction, types::Json as SqlJson};

const MODEL_MAPPING_AUTOMATION_KEY: &str = "channel_model_mapping_automation_enabled";
const MODEL_COLUMNS: &str = "CAST(id AS TEXT) AS id,name,status,developer,model_id,\"type\",icon,\"group\",model_card,settings,remark,created_at,updated_at,CASE WHEN deleted_at=0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug)]
pub(crate) struct PricingAuditSettings {
    pub(crate) currency: String,
    pub(crate) version: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct PgCommercializationAdapter {
    pool: PgPool,
}

impl PgCommercializationAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) async fn pricing_audit_settings(
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<PricingAuditSettings, gql::CommercializationError> {
        let raw = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key='system_general_settings' AND deleted_at=0 LIMIT 1",
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(storage)?;
        let value = raw
            .map(|raw| serde_json::from_str::<serde_json::Value>(&raw))
            .transpose()
            .map_err(|error| {
                gql::CommercializationError::Invalid(format!(
                    "invalid system general settings: {error}"
                ))
            })?
            .unwrap_or_default();
        let currency = value
            .get("accounting_currency_code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(DEFAULT_ACCOUNTING_CURRENCY_CODE)
            .trim()
            .to_ascii_uppercase();
        if currency.len() == 3
            && currency
                .chars()
                .all(|character| character.is_ascii_alphabetic())
        {
            let version = value
                .get("accounting_rate_version")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1);
            if version == 0 {
                return Err(gql::CommercializationError::Invalid(
                    "accounting settings version must be positive".into(),
                ));
            }
            Ok(PricingAuditSettings { currency, version })
        } else {
            Err(gql::CommercializationError::Invalid(
                "accounting currency must be a 3-letter ISO code".into(),
            ))
        }
    }

    async fn model_mapping_automation_enabled(&self) -> Result<bool, gql::CommercializationError> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 LIMIT 1",
        )
        .bind(MODEL_MAPPING_AUTOMATION_KEY)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        Ok(value
            .as_deref()
            .is_some_and(|value| matches!(value, "true" | "1")))
    }

    async fn channel_mapping_preview(
        &self,
        channel_id_value: i64,
    ) -> Result<gql::ChannelModelMappingPreview, gql::CommercializationError> {
        if !self.model_mapping_automation_enabled().await? {
            return Err(gql::CommercializationError::Invalid(
                "automatic channel model mapping is disabled in settings".into(),
            ));
        }
        let row = sqlx::query(
            "SELECT supported_models,settings,updated_at FROM channels \
             WHERE id=$1 AND deleted_at=0",
        )
        .bind(channel_id_value)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            gql::CommercializationError::NotFound(format!("channel {channel_id_value}"))
        })?;
        let supported_models = row
            .try_get::<SqlJson<Vec<String>>, _>("supported_models")
            .map_err(|error| {
                gql::CommercializationError::Storage(format!(
                    "channel supported models are invalid: {error}"
                ))
            })?
            .0;
        let settings = row
            .try_get::<Option<SqlJson<
                conduit_core::objects::channel_settings::ChannelSettings,
            >>, _>("settings")
            .map_err(|error| {
                gql::CommercializationError::Storage(format!(
                    "channel settings are invalid: {error}"
                ))
            })?
            .map(|settings| settings.0)
            .unwrap_or_default();
        let route_rows = sqlx::query(
            "SELECT m.model_id AS public_model_key,d.upstream_model_id \
             FROM model_routes r \
             JOIN models m ON m.id=r.public_model_id AND m.deleted_at=0 AND m.status='enabled' \
             JOIN upstream_model_deployments d ON d.id=r.deployment_id AND d.status='enabled' \
             WHERE d.channel_id=$1 AND r.status='enabled' \
             ORDER BY LOWER(m.model_id),r.id",
        )
        .bind(channel_id_value)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let routed = route_rows
            .into_iter()
            .map(|row| (row.get("public_model_key"), row.get("upstream_model_id")))
            .collect::<Vec<(String, String)>>();
        if let Some((from, to)) = routed
            .iter()
            .find(|(_, target)| !supported_models.iter().any(|model| model == target))
        {
            return Err(gql::CommercializationError::Invalid(format!(
                "route {from:?} points to {to:?}, which is not in this channel's supported models; sync the channel models before applying aliases"
            )));
        }
        let existing = settings
            .model_mappings
            .iter()
            .map(|mapping| conduit_admin_graphql::channel::ModelMapping {
                from: mapping.from.clone(),
                to: mapping.to.clone(),
            })
            .collect::<Vec<_>>();
        Ok(gql::build_channel_model_mapping_preview(
            channel_id(channel_id_value),
            wire_time(row.get("updated_at")),
            &existing,
            &routed,
        ))
    }

    async fn route(&self, route_id: i64) -> Result<gql::ModelRoute, gql::CommercializationError> {
        let row = sqlx::query(
            "SELECT r.id,r.public_model_id,m.model_id AS public_model_key,r.deployment_id, \
                    d.internal_name AS deployment_name,d.channel_id,c.name AS channel_name, \
                    d.upstream_model_id,r.status \
             FROM model_routes r \
             JOIN models m ON m.id=r.public_model_id AND m.deleted_at=0 \
             JOIN upstream_model_deployments d ON d.id=r.deployment_id \
             JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 WHERE r.id=$1",
        )
        .bind(route_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .ok_or_else(|| gql::CommercializationError::NotFound(format!("route {route_id}")))?;
        Ok(route_from_row(row))
    }

    async fn price_book(
        &self,
        book_id: i64,
    ) -> Result<gql::PriceBook, gql::CommercializationError> {
        let row =
            sqlx::query("SELECT id,name,currency,status,is_default FROM price_books WHERE id=$1")
                .bind(book_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?
                .ok_or_else(|| {
                    gql::CommercializationError::NotFound(format!("price book {book_id}"))
                })?;
        Ok(gql::PriceBook {
            id: id(book_id),
            name: row.get("name"),
            currency: row.get("currency"),
            status: status_from_wire(row.get("status")),
            is_default: row.get("is_default"),
            versions: self.price_book_versions(book_id).await?,
        })
    }

    async fn price_book_versions(
        &self,
        book_id: i64,
    ) -> Result<Vec<gql::PriceBookVersion>, gql::CommercializationError> {
        let rows = sqlx::query(
            "SELECT id,version,status,reference_id,effective_start_at,effective_end_at \
             FROM price_book_versions WHERE price_book_id=$1 ORDER BY version DESC",
        )
        .bind(book_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut versions = Vec::with_capacity(rows.len());
        for row in rows {
            let version_id: i64 = row.get("id");
            versions.push(gql::PriceBookVersion {
                id: id(version_id),
                version: i32::try_from(row.get::<i64, _>("version")).map_err(|_| {
                    gql::CommercializationError::Storage("price version exceeds i32".into())
                })?,
                status: row.get("status"),
                reference_id: row.get("reference_id"),
                effective_start_at: optional_wire_time(row.get("effective_start_at")),
                effective_end_at: optional_wire_time(row.get("effective_end_at")),
                items: self.price_book_items(version_id).await?,
            });
        }
        Ok(versions)
    }

    async fn price_book_items(
        &self,
        version_id: i64,
    ) -> Result<Vec<gql::PriceBookItem>, gql::CommercializationError> {
        sqlx::query(
            "SELECT i.id,i.public_model_id,m.model_id AS public_model_key,i.price \
             FROM price_book_items i JOIN models m ON m.id=i.public_model_id \
             WHERE i.price_book_version_id=$1 ORDER BY LOWER(m.model_id),m.id",
        )
        .bind(version_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(price_item_from_row)
        .collect()
    }
}

/// Shared PostgreSQL primitive used by the simple-mode model-group adapter.
pub(crate) async fn create_access_plan_record_postgres(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    description: Option<&str>,
    status: &str,
    is_default: bool,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO access_plans(name,description,status,is_default,created_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$5) RETURNING id",
    )
    .bind(name)
    .bind(description)
    .bind(status)
    .bind(is_default)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}

/// Publish a complete Access Plan snapshot without collapsing deployments that
/// happen to expose the same upstream model name.
pub(crate) async fn publish_access_plan_version_postgres(
    tx: &mut Transaction<'_, Postgres>,
    plan_id: i64,
    model_ids: &[i64],
    route_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    // The plan row serializes version allocation and publication.
    sqlx::query_scalar::<_, i64>("SELECT id FROM access_plans WHERE id=$1 FOR UPDATE")
        .bind(plan_id)
        .fetch_one(&mut **tx)
        .await?;
    let version = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(version),0)+1 FROM access_plan_versions WHERE access_plan_id=$1",
    )
    .bind(plan_id)
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE access_plan_versions SET status='archived',effective_end_at=$1,updated_at=$1 \
         WHERE access_plan_id=$2 AND status='published'",
    )
    .bind(now)
    .bind(plan_id)
    .execute(&mut **tx)
    .await?;
    let version_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO access_plan_versions \
         (access_plan_id,version,status,reference_id,effective_start_at,created_at,updated_at) \
         VALUES($1,$2,'published',$3,$4,$4,$4) RETURNING id",
    )
    .bind(plan_id)
    .bind(version)
    .bind(format!("access-plan-{plan_id}-v{version}"))
    .bind(now)
    .fetch_one(&mut **tx)
    .await?;
    for model_id in model_ids.iter().copied().collect::<BTreeSet<_>>() {
        sqlx::query(
            "INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) \
             VALUES($1,$2,$3)",
        )
        .bind(version_id)
        .bind(model_id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    for route_id in route_ids.iter().copied().collect::<BTreeSet<_>>() {
        sqlx::query(
            "INSERT INTO access_plan_route_items(access_plan_version_id,model_route_id,created_at) \
             VALUES($1,$2,$3)",
        )
        .bind(version_id)
        .bind(route_id)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(version_id)
}

pub(crate) async fn create_price_tier_record_postgres(
    tx: &mut Transaction<'_, Postgres>,
    name: &str,
    multiplier_ppm: i64,
    status: &str,
    is_default: bool,
    now: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO price_tiers(name,multiplier_ppm,status,is_default,created_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$5) RETURNING id",
    )
    .bind(name)
    .bind(multiplier_ppm)
    .bind(status)
    .bind(is_default)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
}

#[async_trait::async_trait]
impl gql::CommercializationServices for PgCommercializationAdapter {
    async fn primary_project_for_user(
        &self,
        user_id: &str,
    ) -> Result<gql::PrimaryProjectResolution, gql::CommercializationError> {
        let user_id = parse_id(user_id)?;
        let candidates = sqlx::query_scalar::<_, i64>(
            "SELECT p.id FROM user_projects up \
             JOIN users u ON u.id=up.user_id \
             JOIN projects p ON p.id=up.project_id \
             JOIN project_commercial_profiles cp ON cp.project_id=p.id \
             WHERE up.user_id=$1 AND up.is_owner=TRUE \
               AND u.status='activated' AND u.deleted_at=0 \
               AND p.status='active' AND p.deleted_at=0 \
               AND cp.account_type='personal' AND cp.status='active' ORDER BY p.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let status = match candidates.len() {
            0 => gql::PrimaryProjectResolutionStatus::Missing,
            1 => gql::PrimaryProjectResolutionStatus::Resolved,
            _ => gql::PrimaryProjectResolutionStatus::Ambiguous,
        };
        Ok(gql::PrimaryProjectResolution {
            status,
            project_id: (candidates.len() == 1).then(|| project_node_id(candidates[0])),
            candidate_project_ids: candidates.into_iter().map(project_node_id).collect(),
        })
    }

    async fn upstream_model_deployments(
        &self,
        channel_filter: Option<&str>,
    ) -> Result<Vec<gql::UpstreamModelDeployment>, gql::CommercializationError> {
        let rows = if let Some(channel_filter) = channel_filter {
            sqlx::query(
                "SELECT d.id,d.channel_id,c.name AS channel_name,d.upstream_model_id, \
                        d.internal_name,d.variant,d.status,d.source \
                 FROM upstream_model_deployments d \
                 JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
                 WHERE d.channel_id=$1 ORDER BY LOWER(d.internal_name),d.id",
            )
            .bind(parse_id(channel_filter)?)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
        } else {
            sqlx::query(
                "SELECT d.id,d.channel_id,c.name AS channel_name,d.upstream_model_id, \
                        d.internal_name,d.variant,d.status,d.source \
                 FROM upstream_model_deployments d \
                 JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
                 ORDER BY LOWER(c.name),LOWER(d.internal_name),d.id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
        };
        Ok(rows.into_iter().map(deployment_from_row).collect())
    }

    async fn model_routes(
        &self,
        public_model_filter: Option<&str>,
    ) -> Result<Vec<gql::ModelRoute>, gql::CommercializationError> {
        let rows = if let Some(public_model_filter) = public_model_filter {
            sqlx::query(
                "SELECT r.id,r.public_model_id,m.model_id AS public_model_key,r.deployment_id, \
                        d.internal_name AS deployment_name,d.channel_id,c.name AS channel_name, \
                        d.upstream_model_id,r.status \
                 FROM model_routes r \
                 JOIN models m ON m.id=r.public_model_id AND m.deleted_at=0 \
                 JOIN upstream_model_deployments d ON d.id=r.deployment_id \
                 JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
                 WHERE r.public_model_id=$1 ORDER BY LOWER(c.name),r.id",
            )
            .bind(parse_id(public_model_filter)?)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
        } else {
            sqlx::query(
                "SELECT r.id,r.public_model_id,m.model_id AS public_model_key,r.deployment_id, \
                        d.internal_name AS deployment_name,d.channel_id,c.name AS channel_name, \
                        d.upstream_model_id,r.status \
                 FROM model_routes r \
                 JOIN models m ON m.id=r.public_model_id AND m.deleted_at=0 \
                 JOIN upstream_model_deployments d ON d.id=r.deployment_id \
                 JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
                 ORDER BY LOWER(m.model_id),LOWER(c.name),r.id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?
        };
        Ok(rows.into_iter().map(route_from_row).collect())
    }

    async fn channel_model_mapping_automation_settings(
        &self,
    ) -> Result<gql::ChannelModelMappingAutomationSettings, gql::CommercializationError> {
        Ok(gql::ChannelModelMappingAutomationSettings {
            enabled: self.model_mapping_automation_enabled().await?,
        })
    }

    async fn set_channel_model_mapping_automation(
        &self,
        input: gql::SetChannelModelMappingAutomationInput,
    ) -> Result<gql::ChannelModelMappingAutomationSettings, gql::CommercializationError> {
        sqlx::query(
            "INSERT INTO systems(key,value,created_at,updated_at,deleted_at) \
             VALUES($1,$2,now(),now(),0) ON CONFLICT(key) DO UPDATE SET \
             value=EXCLUDED.value,updated_at=EXCLUDED.updated_at,deleted_at=0",
        )
        .bind(MODEL_MAPPING_AUTOMATION_KEY)
        .bind(if input.enabled { "true" } else { "false" })
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(gql::ChannelModelMappingAutomationSettings {
            enabled: input.enabled,
        })
    }

    async fn preview_channel_model_mappings(
        &self,
        channel_id_value: &str,
    ) -> Result<gql::ChannelModelMappingPreview, gql::CommercializationError> {
        self.channel_mapping_preview(parse_id(channel_id_value)?)
            .await
    }

    async fn apply_channel_model_mappings(
        &self,
        input: gql::ApplyChannelModelMappingsInput,
    ) -> Result<gql::ChannelModelMappingPreview, gql::CommercializationError> {
        let channel_id_value = parse_id(input.channel_id.as_str())?;
        let preview = self.channel_mapping_preview(channel_id_value).await?;
        if preview.expected_version != input.expected_version {
            return Err(gql::CommercializationError::Invalid(
                "channel changed after the preview; generate a new preview before applying".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let row = sqlx::query(
            "SELECT settings,updated_at FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(channel_id_value)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            gql::CommercializationError::NotFound(format!("channel {channel_id_value}"))
        })?;
        if wire_time(row.get("updated_at")) != input.expected_version {
            return Err(gql::CommercializationError::Invalid(
                "channel changed while aliases were being applied; generate a new preview".into(),
            ));
        }
        let mut settings = row
            .try_get::<Option<SqlJson<
                conduit_core::objects::channel_settings::ChannelSettings,
            >>, _>("settings")
            .map_err(|error| {
                gql::CommercializationError::Storage(format!(
                    "channel settings are invalid: {error}"
                ))
            })?
            .map(|settings| settings.0)
            .unwrap_or_default();
        let existing = settings
            .model_mappings
            .iter()
            .map(|mapping| conduit_admin_graphql::channel::ModelMapping {
                from: mapping.from.clone(),
                to: mapping.to.clone(),
            })
            .collect::<Vec<_>>();
        settings.model_mappings = gql::merge_channel_model_mappings(
            &existing,
            &preview,
            input.replace_conflicts.unwrap_or(false),
        )
        .into_iter()
        .map(
            |mapping| conduit_core::objects::channel_settings::ModelMapping {
                from: mapping.from,
                to: mapping.to,
            },
        )
        .collect();
        sqlx::query("UPDATE channels SET settings=$1,updated_at=$2 WHERE id=$3")
            .bind(SqlJson(settings))
            .bind(Utc::now())
            .bind(channel_id_value)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.channel_mapping_preview(channel_id_value).await
    }

    async fn upsert_model_route(
        &self,
        input: gql::UpsertModelRouteInput,
    ) -> Result<gql::ModelRoute, gql::CommercializationError> {
        let public_model_id = parse_id(input.public_model_id.as_str())?;
        let deployment_id = parse_id(input.deployment_id.as_str())?;
        let editing_route_id = input
            .id
            .as_ref()
            .map(|value| parse_id(value.as_str()))
            .transpose()?;
        let route_status = status_to_wire(input.status.unwrap_or(gql::CommercialStatus::Enabled));
        let mut tx = self.pool.begin().await.map_err(storage)?;
        if sqlx::query_scalar::<_, i64>(
            "SELECT id FROM models WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(public_model_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .is_none()
        {
            return Err(gql::CommercializationError::NotFound(format!(
                "public model SKU {public_model_id}"
            )));
        }
        let selected = sqlx::query(
            "SELECT channel_id,status FROM upstream_model_deployments WHERE id=$1 FOR SHARE",
        )
        .bind(deployment_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        .ok_or_else(|| {
            gql::CommercializationError::NotFound(format!("deployment {deployment_id}"))
        })?;
        if route_status == "enabled" && selected.get::<String, _>("status") != "enabled" {
            return Err(gql::CommercializationError::Invalid(
                "an enabled route requires an enabled upstream deployment".into(),
            ));
        }
        let selected_channel_id: i64 = selected.get("channel_id");
        let (target_route_id, old_channel_id) = if let Some(route_id) = editing_route_id {
            let old_channel_id = sqlx::query_scalar::<_, i64>(
                "SELECT d.channel_id FROM model_routes r \
                 JOIN upstream_model_deployments d ON d.id=r.deployment_id \
                 WHERE r.id=$1 FOR UPDATE OF r",
            )
            .bind(route_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .ok_or_else(|| gql::CommercializationError::NotFound(format!("route {route_id}")))?;
            (Some(route_id), Some(old_channel_id))
        } else {
            let existing = sqlx::query(
                "SELECT r.id,d.channel_id FROM model_routes r \
                 JOIN upstream_model_deployments d ON d.id=r.deployment_id \
                 WHERE r.public_model_id=$1 AND r.deployment_id=$2 FOR UPDATE OF r",
            )
            .bind(public_model_id)
            .bind(deployment_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            existing
                .map(|row| (Some(row.get("id")), Some(row.get("channel_id"))))
                .unwrap_or((None, None))
        };
        let same_channel = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM model_routes r \
             JOIN upstream_model_deployments existing ON existing.id=r.deployment_id \
             WHERE r.public_model_id=$1 AND existing.channel_id=$2 \
               AND ($3::BIGINT IS NULL OR r.id<>$3) AND r.status<>'archived'",
        )
        .bind(public_model_id)
        .bind(selected_channel_id)
        .bind(target_route_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        if same_channel > 0 {
            return Err(gql::CommercializationError::Invalid(
                "a public model SKU can have only one route per channel; create a separate SKU for another deployment on the same channel".into(),
            ));
        }
        let other_routes = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM model_routes WHERE public_model_id=$1 \
             AND ($2::BIGINT IS NULL OR id<>$2) AND status<>'archived'",
        )
        .bind(public_model_id)
        .bind(target_route_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        if other_routes > 0 && input.confirm_compatibility != Some(true) {
            return Err(gql::CommercializationError::Invalid(
                "this SKU already has another route; confirm compatibility (equivalent capability, quality and price) or create a separate SKU".into(),
            ));
        }
        let now = Utc::now();
        let route_id = if let Some(route_id) = target_route_id {
            sqlx::query(
                "UPDATE model_routes SET public_model_id=$1,deployment_id=$2,status=$3,updated_at=$4 \
                 WHERE id=$5",
            )
            .bind(public_model_id)
            .bind(deployment_id)
            .bind(route_status)
            .bind(now)
            .bind(route_id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            route_id
        } else {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO model_routes(public_model_id,deployment_id,status,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,$4) ON CONFLICT(public_model_id,deployment_id) \
                 DO UPDATE SET status=EXCLUDED.status,updated_at=EXCLUDED.updated_at RETURNING id",
            )
            .bind(public_model_id)
            .bind(deployment_id)
            .bind(route_status)
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?
        };
        let mut affected_channels = BTreeSet::from([selected_channel_id]);
        affected_channels.extend(old_channel_id);
        sqlx::query("UPDATE channels SET updated_at=$1 WHERE id=ANY($2)")
            .bind(now)
            .bind(affected_channels.into_iter().collect::<Vec<_>>())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.route(route_id).await
    }

    async fn create_public_model_with_routes(
        &self,
        input: gql::CreatePublicModelWithRoutesInput,
    ) -> Result<gql::CreatePublicModelWithRoutesPayload, gql::CommercializationError> {
        if input.deployment_ids.is_empty() {
            return Err(gql::CommercializationError::Invalid(
                "select at least one upstream model deployment".into(),
            ));
        }
        if input.deployment_ids.len() > 1 && input.confirm_compatibility != Some(true) {
            return Err(gql::CommercializationError::Invalid(
                "multiple upstream deployments require explicit compatibility confirmation".into(),
            ));
        }
        let deployment_ids = input
            .deployment_ids
            .iter()
            .map(|value| parse_id(value.as_str()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if deployment_ids.len() != input.deployment_ids.len() {
            return Err(gql::CommercializationError::Invalid(
                "the same upstream deployment cannot be selected twice".into(),
            ));
        }
        let model_key = nonempty(input.model.model_id.clone(), "modelID")?;
        let name = nonempty(input.model.name.clone(), "name")?;
        let columns = crate::conv::create_model_columns(&input.model);
        let model_status = if input.enabled.unwrap_or(true) {
            "enabled"
        } else {
            "disabled"
        };
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let mut channel_ids = BTreeSet::new();
        for deployment_id in &deployment_ids {
            let deployment = sqlx::query(
                "SELECT channel_id,status FROM upstream_model_deployments WHERE id=$1 FOR SHARE",
            )
            .bind(deployment_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?
            .ok_or_else(|| {
                gql::CommercializationError::NotFound(format!(
                    "upstream deployment {deployment_id}"
                ))
            })?;
            if deployment.get::<String, _>("status") != "enabled" {
                return Err(gql::CommercializationError::Invalid(format!(
                    "upstream deployment {deployment_id} is not enabled"
                )));
            }
            if !channel_ids.insert(deployment.get::<i64, _>("channel_id")) {
                return Err(gql::CommercializationError::Invalid(
                    "select at most one upstream deployment from each channel".into(),
                ));
            }
        }
        if let Some(conflict) = sqlx::query(
            "SELECT model_id,name FROM models \
             WHERE deleted_at=0 AND (model_id=$1 OR name=$2) LIMIT 1 FOR SHARE",
        )
        .bind(&model_key)
        .bind(&name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?
        {
            let field = if conflict.get::<String, _>("model_id") == model_key {
                "model ID"
            } else {
                "display name"
            };
            return Err(gql::CommercializationError::Invalid(format!(
                "{field} already exists"
            )));
        }
        let public_model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO models \
             (developer,model_id,\"type\",name,icon,\"group\",model_card,settings,status, \
              remark,created_at,updated_at,deleted_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11,0) RETURNING id",
        )
        .bind(&input.model.developer)
        .bind(&model_key)
        .bind(&columns.model_type)
        .bind(&name)
        .bind(&input.model.icon)
        .bind(&input.model.group)
        .bind(SqlJson(columns.model_card))
        .bind(SqlJson(columns.settings))
        .bind(model_status)
        .bind(&input.model.remark)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        for deployment_id in &deployment_ids {
            sqlx::query(
                "INSERT INTO model_routes(public_model_id,deployment_id,status,created_at,updated_at) \
                 VALUES($1,$2,$3,$4,$4)",
            )
            .bind(public_model_id)
            .bind(deployment_id)
            .bind(model_status)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        }
        sqlx::query("UPDATE channels SET updated_at=$1 WHERE id=ANY($2)")
            .bind(now)
            .bind(channel_ids.into_iter().collect::<Vec<_>>())
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        let model = sqlx::query_as::<_, ModelRow>(&format!(
            "SELECT {MODEL_COLUMNS} FROM models WHERE id=$1"
        ))
        .bind(public_model_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let routes = sqlx::query(
            "SELECT r.id,r.public_model_id,m.model_id AS public_model_key,r.deployment_id, \
                    d.internal_name AS deployment_name,d.channel_id,c.name AS channel_name, \
                    d.upstream_model_id,r.status \
             FROM model_routes r JOIN models m ON m.id=r.public_model_id AND m.deleted_at=0 \
             JOIN upstream_model_deployments d ON d.id=r.deployment_id \
             JOIN channels c ON c.id=d.channel_id AND c.deleted_at=0 \
             WHERE r.public_model_id=$1 ORDER BY LOWER(c.name),r.id",
        )
        .bind(public_model_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(gql::CreatePublicModelWithRoutesPayload {
            model: crate::conv::model_row_to_gql(model),
            routes: routes.into_iter().map(route_from_row).collect(),
        })
    }

    async fn price_books(&self) -> Result<Vec<gql::PriceBook>, gql::CommercializationError> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM price_books ORDER BY is_default DESC,LOWER(name),id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        let mut books = Vec::with_capacity(ids.len());
        for book_id in ids {
            books.push(self.price_book(book_id).await?);
        }
        Ok(books)
    }

    async fn create_price_book(
        &self,
        actor_user_id: Option<i64>,
        input: gql::CreatePriceBookInput,
    ) -> Result<gql::PriceBook, gql::CommercializationError> {
        let name = nonempty(input.name, "name")?;
        let is_default = input.is_default.unwrap_or(false);
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(storage)?;
        let audit_settings = Self::pricing_audit_settings(&mut tx).await?;
        let accounting_currency = &audit_settings.currency;
        let currency = input
            .currency
            .unwrap_or_else(|| accounting_currency.to_string())
            .trim()
            .to_ascii_uppercase();
        if currency != *accounting_currency {
            return Err(gql::CommercializationError::Invalid(format!(
                "retail price books must use accounting currency {accounting_currency}"
            )));
        }
        let before_snapshot = price_book_catalog_snapshot(&mut tx).await?;
        if is_default {
            sqlx::query("UPDATE price_books SET is_default=FALSE,updated_at=$1")
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        let book_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO price_books(name,currency,status,is_default,created_at,updated_at) \
             VALUES($1,$2,'enabled',$3,$4,$4) RETURNING id",
        )
        .bind(name)
        .bind(currency)
        .bind(is_default)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let after_snapshot = price_book_catalog_snapshot(&mut tx).await?;
        insert_pricing_audit(
            &mut tx,
            actor_user_id,
            "create_price_book",
            "price_book",
            &book_id.to_string(),
            Some(before_snapshot),
            Some(after_snapshot),
            &audit_settings,
        )
        .await?;
        tx.commit().await.map_err(storage)?;
        self.price_book(book_id).await
    }
}

pub(crate) async fn price_book_state_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    book_id: i64,
) -> Result<serde_json::Value, gql::CommercializationError> {
    sqlx::query_scalar::<_, SqlJson<serde_json::Value>>(
        "SELECT jsonb_build_object( \
             'priceBook',to_jsonb(b), \
             'versions',COALESCE(( \
               SELECT jsonb_agg( \
                 to_jsonb(v) || jsonb_build_object( \
                   'items',COALESCE(( \
                     SELECT jsonb_agg(to_jsonb(i) ORDER BY i.id) \
                     FROM price_book_items i WHERE i.price_book_version_id=v.id \
                   ),'[]'::jsonb) \
                 ) ORDER BY v.version,v.id \
               ) FROM price_book_versions v WHERE v.price_book_id=b.id \
             ),'[]'::jsonb) \
           ) FROM price_books b WHERE b.id=$1",
    )
    .bind(book_id)
    .fetch_one(&mut **tx)
    .await
    .map(|SqlJson(value)| value)
    .map_err(storage)
}

async fn price_book_catalog_snapshot(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<serde_json::Value, gql::CommercializationError> {
    let book_ids = sqlx::query_scalar::<_, i64>("SELECT id FROM price_books ORDER BY id")
        .fetch_all(&mut **tx)
        .await
        .map_err(storage)?;
    let mut books = Vec::with_capacity(book_ids.len());
    for book_id in book_ids {
        books.push(price_book_state_snapshot(tx, book_id).await?);
    }
    Ok(serde_json::Value::Array(books))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_pricing_audit(
    tx: &mut Transaction<'_, Postgres>,
    actor_user_id: Option<i64>,
    operation: &str,
    entity_type: &str,
    entity_id: &str,
    before_snapshot: Option<serde_json::Value>,
    after_snapshot: Option<serde_json::Value>,
    settings: &PricingAuditSettings,
) -> Result<(), gql::CommercializationError> {
    sqlx::query(
        "INSERT INTO pricing_change_audits \
         (actor_type,actor_id,operation,entity_type,entity_id,before_snapshot,after_snapshot, \
          accounting_currency,accounting_settings_version,result,request_correlation_id,created_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'success',$10,$11)",
    )
    .bind(if actor_user_id.is_some() {
        "user"
    } else {
        "system"
    })
    .bind(actor_user_id)
    .bind(operation)
    .bind(entity_type)
    .bind(entity_id)
    .bind(before_snapshot)
    .bind(after_snapshot)
    .bind(&settings.currency)
    .bind(i64::try_from(settings.version).unwrap_or(i64::MAX))
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(Utc::now())
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    Ok(())
}

fn deployment_from_row(row: sqlx::postgres::PgRow) -> gql::UpstreamModelDeployment {
    gql::UpstreamModelDeployment {
        id: id(row.get("id")),
        channel_id: channel_id(row.get("channel_id")),
        channel_name: row.get("channel_name"),
        upstream_model_id: row.get("upstream_model_id"),
        internal_name: row.get("internal_name"),
        variant: row.get("variant"),
        status: status_from_wire(row.get("status")),
        source: row.get("source"),
    }
}

fn route_from_row(row: sqlx::postgres::PgRow) -> gql::ModelRoute {
    gql::ModelRoute {
        id: id(row.get("id")),
        public_model_id: model_id(row.get("public_model_id")),
        public_model_key: row.get("public_model_key"),
        deployment_id: id(row.get("deployment_id")),
        deployment_name: row.get("deployment_name"),
        channel_id: channel_id(row.get("channel_id")),
        channel_name: row.get("channel_name"),
        upstream_model_id: row.get("upstream_model_id"),
        status: status_from_wire(row.get("status")),
    }
}

fn price_item_from_row(
    row: sqlx::postgres::PgRow,
) -> Result<gql::PriceBookItem, gql::CommercializationError> {
    let price = row
        .try_get::<SqlJson<serde_json::Value>, _>("price")
        .map_err(|error| {
            gql::CommercializationError::Storage(format!("stored price is invalid: {error}"))
        })?
        .0;
    serde_json::from_value::<ModelPrice>(price.clone()).map_err(|error| {
        gql::CommercializationError::Storage(format!("stored price is invalid: {error}"))
    })?;
    Ok(gql::PriceBookItem {
        id: id(row.get("id")),
        public_model_id: model_id(row.get("public_model_id")),
        public_model_key: row.get("public_model_key"),
        price: Json(price),
    })
}

fn parse_id(value: &str) -> Result<i64, gql::CommercializationError> {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .parse()
        .map_err(|_| gql::CommercializationError::Invalid(format!("invalid id {value:?}")))
}

fn nonempty(value: String, field: &str) -> Result<String, gql::CommercializationError> {
    let value = value.trim();
    if value.is_empty() {
        Err(gql::CommercializationError::Invalid(format!(
            "{field} cannot be empty"
        )))
    } else {
        Ok(value.to_string())
    }
}

fn id(value: i64) -> ID {
    ID(value.to_string())
}

fn model_id(value: i64) -> ID {
    node_id("Model", value)
}

fn channel_id(value: i64) -> ID {
    node_id("Channel", value)
}

fn project_node_id(value: i64) -> ID {
    node_id("Project", value)
}

fn node_id(kind: &str, value: i64) -> ID {
    ID(format!("gid://conduit/{kind}/{value}"))
}

fn storage(error: sqlx::Error) -> gql::CommercializationError {
    gql::CommercializationError::Storage(error.to_string())
}

fn status_to_wire(status: gql::CommercialStatus) -> &'static str {
    match status {
        gql::CommercialStatus::Enabled => "enabled",
        gql::CommercialStatus::Disabled => "disabled",
        gql::CommercialStatus::Archived => "archived",
    }
}

fn status_from_wire(status: String) -> gql::CommercialStatus {
    match status.as_str() {
        "disabled" => gql::CommercialStatus::Disabled,
        "archived" => gql::CommercialStatus::Archived,
        _ => gql::CommercialStatus::Enabled,
    }
}

fn wire_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn optional_wire_time(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(wire_time)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::change_set::ChangeSetServices as _;
    use conduit_admin_graphql::commercialization::CommercializationServices as _;
    use sqlx::types::Json as SqlJson;

    fn public_model_input(
        model_key: &str,
        deployments: Vec<i64>,
    ) -> gql::CreatePublicModelWithRoutesInput {
        gql::CreatePublicModelWithRoutesInput {
            model: conduit_admin_graphql::model::CreateModelInput {
                developer: "commercial-test".into(),
                model_id: model_key.into(),
                model_type: Some(conduit_admin_graphql::model::ModelType::Chat),
                name: format!("{model_key} display"),
                icon: "Test".into(),
                group: "commercial-test".into(),
                model_card: Default::default(),
                settings: Default::default(),
                remark: None,
            },
            deployment_ids: deployments.into_iter().map(id).collect(),
            enabled: Some(true),
            confirm_compatibility: Some(true),
        }
    }

    fn price(item_code: &str, unit: &str) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "items": [{
                "itemCode": item_code,
                "pricing": {"mode": "usage_per_unit", "usagePerUnit": unit}
            }]
        }))
    }

    #[tokio::test]
    async fn postgres_commercialization_preserves_deployment_identity_and_price_snapshots_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let channel_a = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai',$1,'enabled','{}'::jsonb,$2,'same-upstream','{}'::jsonb) \
             RETURNING id",
        )
        .bind(format!("PG Commercial A {suffix}"))
        .bind(SqlJson(vec![
            "same-upstream".to_string(),
            "lower-upstream".to_string(),
        ]))
        .fetch_one(&pool)
        .await?;
        let channel_b = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai',$1,'enabled','{}'::jsonb,$2,'same-upstream','{}'::jsonb) \
             RETURNING id",
        )
        .bind(format!("PG Commercial B {suffix}"))
        .bind(SqlJson(vec!["same-upstream".to_string()]))
        .fetch_one(&pool)
        .await?;
        let deployment =
            |channel_id: i64, upstream: &'static str, internal: String, variant: String| {
                let pool = pool.clone();
                async move {
                    sqlx::query_scalar::<_, i64>(
                        "INSERT INTO upstream_model_deployments \
                     (channel_id,upstream_model_id,internal_name,variant,status,source) \
                     VALUES($1,$2,$3,$4,'enabled','test') \
                     ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET \
                       internal_name=EXCLUDED.internal_name,status='enabled',updated_at=now() \
                     RETURNING id",
                    )
                    .bind(channel_id)
                    .bind(upstream)
                    .bind(internal)
                    .bind(variant)
                    .fetch_one(&pool)
                    .await
                }
            };
        let deployment_a = deployment(
            channel_a,
            "same-upstream",
            format!("M1 {suffix}"),
            format!("a-{suffix}"),
        )
        .await?;
        let deployment_b = deployment(
            channel_b,
            "same-upstream",
            format!("M2 {suffix}"),
            format!("b-{suffix}"),
        )
        .await?;
        let lower_a = deployment(
            channel_a,
            "lower-upstream",
            format!("M3 {suffix}"),
            format!("low-{suffix}"),
        )
        .await?;
        let adapter = PgCommercializationAdapter::new(pool.clone());
        let public_one = adapter
            .create_public_model_with_routes(public_model_input(
                &format!("pg-public-one-{suffix}"),
                vec![deployment_a, deployment_b],
            ))
            .await?;
        assert_eq!(public_one.routes.len(), 2);
        assert_eq!(
            public_one
                .routes
                .iter()
                .map(|route| route.channel_id.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([channel_id(channel_a), channel_id(channel_b)])
        );
        assert!(
            public_one
                .routes
                .iter()
                .all(|route| route.upstream_model_id == "same-upstream")
        );
        let same_channel_error = adapter
            .upsert_model_route(gql::UpsertModelRouteInput {
                id: None,
                public_model_id: public_one.model.id.clone(),
                deployment_id: id(lower_a),
                status: Some(gql::CommercialStatus::Enabled),
                confirm_compatibility: Some(true),
            })
            .await
            .expect_err("one public SKU cannot have two deployments on one channel");
        assert!(
            same_channel_error
                .to_string()
                .contains("one route per channel")
        );

        let public_two = adapter
            .create_public_model_with_routes(public_model_input(
                &format!("pg-public-two-{suffix}"),
                vec![lower_a],
            ))
            .await?;
        assert_eq!(public_two.routes.len(), 1);
        let idempotent_route = adapter
            .upsert_model_route(gql::UpsertModelRouteInput {
                id: None,
                public_model_id: public_two.model.id.clone(),
                deployment_id: id(lower_a),
                status: Some(gql::CommercialStatus::Enabled),
                confirm_compatibility: None,
            })
            .await?;
        assert_eq!(idempotent_route.id, public_two.routes[0].id);

        adapter
            .set_channel_model_mapping_automation(gql::SetChannelModelMappingAutomationInput {
                enabled: true,
            })
            .await?;
        let mapping_preview = adapter
            .preview_channel_model_mappings(&channel_a.to_string())
            .await?;
        assert_eq!(mapping_preview.create_count, 2);
        let applied = adapter
            .apply_channel_model_mappings(gql::ApplyChannelModelMappingsInput {
                channel_id: id(channel_a),
                expected_version: mapping_preview.expected_version,
                replace_conflicts: Some(false),
            })
            .await?;
        assert_eq!(applied.skip_count, 2);
        let stored_settings = sqlx::query_scalar::<
            _,
            SqlJson<conduit_core::objects::channel_settings::ChannelSettings>,
        >("SELECT settings FROM channels WHERE id=$1")
        .bind(channel_a)
        .fetch_one(&pool)
        .await?
        .0;
        assert_eq!(stored_settings.model_mappings.len(), 2);
        let stale_preview = adapter
            .preview_channel_model_mappings(&channel_a.to_string())
            .await?;
        let second_route = public_two.routes.first().expect("second route");
        adapter
            .upsert_model_route(gql::UpsertModelRouteInput {
                id: Some(second_route.id.clone()),
                public_model_id: public_two.model.id.clone(),
                deployment_id: second_route.deployment_id.clone(),
                status: Some(gql::CommercialStatus::Disabled),
                confirm_compatibility: None,
            })
            .await?;
        let stale_apply = adapter
            .apply_channel_model_mappings(gql::ApplyChannelModelMappingsInput {
                channel_id: id(channel_a),
                expected_version: stale_preview.expected_version,
                replace_conflicts: Some(false),
            })
            .await
            .expect_err("route edits must invalidate a mapping preview");
        assert!(
            stale_apply
                .to_string()
                .contains("changed after the preview")
        );
        adapter
            .upsert_model_route(gql::UpsertModelRouteInput {
                id: Some(second_route.id.clone()),
                public_model_id: public_two.model.id.clone(),
                deployment_id: second_route.deployment_id.clone(),
                status: Some(gql::CommercialStatus::Enabled),
                confirm_compatibility: None,
            })
            .await?;

        let book = adapter
            .create_price_book(
                None,
                gql::CreatePriceBookInput {
                    name: format!("PG Retail {suffix}"),
                    currency: Some(DEFAULT_ACCOUNTING_CURRENCY_CODE.into()),
                    // This opt-in integration test runs against the database from
                    // CONDUIT_TEST_POSTGRES_DSN. It must not replace that
                    // database's operational default price book: doing so makes
                    // unrelated gateway requests unpriced after the test exits.
                    is_default: Some(false),
                },
            )
            .await?;
        let change_sets = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
        let first_change_set = change_sets
            .create_retail_price_change_set(1, book.id.clone())
            .await?;
        change_sets
            .save_retail_price_change_set_item(
                1,
                conduit_admin_graphql::change_set::SaveRetailPriceChangeSetItemInput {
                    change_set_id: first_change_set.id.clone(),
                    public_model_id: public_one.model.id.clone(),
                    price: price("prompt_tokens", "1.25"),
                },
            )
            .await?;
        change_sets
            .submit_change_set(1, first_change_set.id.clone())
            .await?;
        change_sets
            .approve_change_set(1, first_change_set.id.clone(), None)
            .await?;
        let second_change_set = change_sets
            .create_retail_price_change_set(1, book.id.clone())
            .await?;
        assert_eq!(second_change_set.items.len(), 1);
        change_sets
            .save_retail_price_change_set_item(
                1,
                conduit_admin_graphql::change_set::SaveRetailPriceChangeSetItemInput {
                    change_set_id: second_change_set.id.clone(),
                    public_model_id: public_two.model.id.clone(),
                    price: price("completion_tokens", "2.50"),
                },
            )
            .await?;
        change_sets
            .submit_change_set(1, second_change_set.id.clone())
            .await?;
        change_sets
            .approve_change_set(1, second_change_set.id.clone(), None)
            .await?;
        let published = adapter.price_book(parse_id(book.id.as_str())?).await?;
        let published = published
            .versions
            .iter()
            .find(|version| version.status == "published")
            .expect("published retail version");
        assert_eq!(published.items.len(), 2);
        assert_eq!(
            published
                .items
                .iter()
                .map(|item| item.public_model_id.clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([public_one.model.id.clone(), public_two.model.id.clone()])
        );

        // Concurrent editor requests serialize on the price book and return
        // the same inherited retail-price change set.
        let mut draft_tasks = Vec::new();
        for _ in 0..6 {
            let change_sets =
                crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
            let book_id = book.id.clone();
            draft_tasks.push(tokio::spawn(async move {
                change_sets.create_retail_price_change_set(1, book_id).await
            }));
        }
        let mut draft_ids = BTreeSet::new();
        for task in draft_tasks {
            let draft = task.await??;
            assert_eq!(draft.items.len(), 2);
            draft_ids.insert(draft.id);
        }
        assert_eq!(draft_ids.len(), 1);
        let audit_operations = sqlx::query_as::<_, (String, i64)>(
            "SELECT operation,COUNT(*) FROM pricing_change_audits \
             WHERE entity_type IN ('price_book','price_book_version','price_book_item') \
             GROUP BY operation ORDER BY operation",
        )
        .fetch_all(&pool)
        .await?
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(audit_operations.get("create_price_book"), Some(&1));
        assert_eq!(
            audit_operations.get("apply_retail_price_change_set"),
            Some(&2)
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pricing_change_audits \
                 WHERE entity_type IN ('price_book','price_book_version','price_book_item') \
                   AND actor_type='system' AND after_snapshot IS NOT NULL",
            )
            .fetch_one(&pool)
            .await?,
            1
        );

        // Exercise the shared Access Plan / price-tier primitives used by the
        // model-group facade, preserving route identity in the snapshot.
        let mut tx = pool.begin().await?;
        let access_plan_id = create_access_plan_record_postgres(
            &mut tx,
            &format!("PG Access {suffix}"),
            Some("commercial test"),
            "enabled",
            false,
            Utc::now(),
        )
        .await?;
        let public_one_id = parse_id(public_one.model.id.as_str())?;
        let public_two_id = parse_id(public_two.model.id.as_str())?;
        let route_ids = public_one
            .routes
            .iter()
            .chain(public_two.routes.iter())
            .map(|route| parse_id(route.id.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let access_version = publish_access_plan_version_postgres(
            &mut tx,
            access_plan_id,
            &[public_one_id, public_two_id],
            &route_ids,
            Utc::now(),
        )
        .await?;
        let price_tier_id = create_price_tier_record_postgres(
            &mut tx,
            &format!("PG Tier {suffix}"),
            1_250_000,
            "enabled",
            false,
            Utc::now(),
        )
        .await?;
        tx.commit().await?;
        assert!(price_tier_id > 0);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM access_plan_items WHERE access_plan_version_id=$1"
            )
            .bind(access_version)
            .fetch_one(&pool)
            .await?,
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM access_plan_route_items WHERE access_plan_version_id=$1"
            )
            .bind(access_version)
            .fetch_one(&pool)
            .await?,
            3
        );

        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,status,prefer_language,password,first_name,last_name,is_owner,scopes) \
             VALUES($1,'activated','en','test','Commercial','User',FALSE,'[]'::jsonb) RETURNING id",
        )
        .bind(format!("pg-commercial-{suffix}@example.com"))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description,status,profiles) \
             VALUES($1,'commercial primary','active','{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Commercial Project {suffix}"))
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
        sqlx::query(
            "INSERT INTO project_commercial_profiles \
             (project_id,account_type,billing_currency,status,created_at,updated_at) \
             VALUES($1,'personal','STATION_CREDIT','active',now(),now())",
        )
        .bind(project_id)
        .execute(&pool)
        .await?;
        let primary = adapter
            .primary_project_for_user(&user_id.to_string())
            .await?;
        assert_eq!(
            primary.status,
            gql::PrimaryProjectResolutionStatus::Resolved
        );
        assert_eq!(primary.project_id, Some(project_node_id(project_id)));
        database.cleanup().await?;
        Ok(())
    }
}

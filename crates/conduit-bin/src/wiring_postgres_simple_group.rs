//! PostgreSQL model-group adapter for the simple-mode façade.
//!
//! A model group owns a versioned Access Plan (public models plus optional
//! concrete Route IDs) and a Price Tier. Route IDs preserve deployment/channel
//! identity when several upstreams expose the same model name.

use std::collections::{BTreeMap, BTreeSet};

use async_graphql::ID;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::scalars::TimeScalar;
use conduit_admin_graphql::simple_group::{
    APIKeyAssignableGroup, AssignSimpleGroupUsersInput, CreateSimpleGroupInput, SimpleGroup,
    SimpleGroupServiceError, SimpleGroupServices, SimpleGroupStatus, UpdateSimpleGroupInput,
    UpdateSimpleGroupModelsInput, UpdateSimpleGroupPriceInput,
};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgSimpleGroupAdapter {
    pool: PgPool,
}

impl PgSimpleGroupAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn load_native_simple_group(
        &self,
        group_id: &str,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let row = sqlx::query(
            "SELECT g.id, g.name, g.description, g.status, g.is_default, g.access_plan_id, \
                    g.price_tier_id, g.default_subscription_plan_id, t.multiplier_ppm, \
                    g.created_at, g.updated_at \
             FROM simple_groups g JOIN price_tiers t ON t.id = g.price_tier_id WHERE g.id = $1",
        )
        .bind(group_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.to_owned()))?;
        let member_project_ids = sqlx::query_scalar::<_, i64>(
            "SELECT project_id FROM simple_group_projects WHERE simple_group_id = $1 ORDER BY project_id",
        )
        .bind(group_id)
        .fetch_all(&self.pool)
        .await
        .map_err(simple_group_storage)?;
        let mut owners_by_project = BTreeMap::<i64, BTreeSet<i64>>::new();
        if !member_project_ids.is_empty() {
            let owner_rows = sqlx::query(
                "SELECT gp.project_id, up.user_id FROM simple_group_projects gp \
                 LEFT JOIN user_projects up ON up.project_id = gp.project_id AND up.is_owner = TRUE \
                 WHERE gp.simple_group_id = $1 ORDER BY gp.project_id, up.user_id",
            )
            .bind(group_id)
            .fetch_all(&self.pool)
            .await
            .map_err(simple_group_storage)?;
            for owner in owner_rows {
                let project_id: i64 = owner.get("project_id");
                let owners = owners_by_project.entry(project_id).or_default();
                if let Some(user_id) = owner.get::<Option<i64>, _>("user_id") {
                    owners.insert(user_id);
                }
            }
        }
        let mut member_user_ids = BTreeSet::new();
        let mut unresolved_member_count = 0;
        for project_id in &member_project_ids {
            match owners_by_project.get(project_id) {
                Some(owners) if owners.len() == 1 => member_user_ids.extend(owners),
                _ => unresolved_member_count += 1,
            }
        }
        let access_plan_id: i64 = row.get("access_plan_id");
        let model_ids = sqlx::query_scalar::<_, i64>(
            "SELECT i.public_model_id FROM access_plan_versions v \
             JOIN access_plan_items i ON i.access_plan_version_id = v.id \
             WHERE v.access_plan_id = $1 AND v.status = 'published' \
               AND v.version = (SELECT MAX(v2.version) FROM access_plan_versions v2 \
                                WHERE v2.access_plan_id = v.access_plan_id AND v2.status = 'published') \
             ORDER BY i.public_model_id",
        )
        .bind(access_plan_id)
        .fetch_all(&self.pool)
        .await
        .map_err(simple_group_storage)?;
        let route_ids = sqlx::query_scalar::<_, i64>(
            "SELECT r.model_route_id FROM access_plan_versions v \
             JOIN access_plan_route_items r ON r.access_plan_version_id = v.id \
             WHERE v.access_plan_id = $1 AND v.status = 'published' \
               AND v.version = (SELECT MAX(v2.version) FROM access_plan_versions v2 \
                                WHERE v2.access_plan_id = v.access_plan_id AND v2.status = 'published') \
             ORDER BY r.model_route_id",
        )
        .bind(access_plan_id)
        .fetch_all(&self.pool)
        .await
        .map_err(simple_group_storage)?;
        simple_group_from_native_row(
            row,
            member_project_ids,
            member_user_ids.into_iter().collect(),
            unresolved_member_count,
            model_ids,
            route_ids,
        )
    }
}

async fn lock_simple_group_mutations(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), SimpleGroupServiceError> {
    // Simple groups share one default marker and move Project membership
    // between rows. A transaction-scoped advisory lock makes those cross-row
    // invariants deterministic under concurrent admin mutations.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('conduit.simple_groups', 0))")
        .execute(&mut **tx)
        .await
        .map_err(simple_group_storage)?;
    Ok(())
}

#[async_trait::async_trait]
impl SimpleGroupServices for PgSimpleGroupAdapter {
    async fn simple_groups(&self) -> Result<Vec<SimpleGroup>, SimpleGroupServiceError> {
        let native_ids = sqlx::query_scalar::<_, String>(
            "SELECT id FROM simple_groups ORDER BY is_default DESC, LOWER(name), id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(simple_group_storage)?;
        let mut groups = Vec::with_capacity(native_ids.len());
        for group_id in native_ids {
            groups.push(self.load_native_simple_group(&group_id).await?);
        }
        Ok(groups)
    }

    async fn api_key_assignable_groups(
        &self,
        project_id: i64,
    ) -> Result<Vec<APIKeyAssignableGroup>, SimpleGroupServiceError> {
        let now = Utc::now();
        let group_ids = sqlx::query_scalar::<_, String>(
            "SELECT g.id FROM simple_groups g \
             WHERE g.status = 'enabled' AND (\
               EXISTS (SELECT 1 FROM simple_group_projects gp \
                       WHERE gp.simple_group_id = g.id AND gp.project_id = $1) \
               OR EXISTS (SELECT 1 FROM project_commercial_profiles p \
                          WHERE p.project_id = $2 AND p.status = 'active' \
                            AND p.base_access_plan_id = g.access_plan_id) \
               OR EXISTS (SELECT 1 FROM project_access_grants pg \
                          JOIN access_plan_versions av ON av.id = pg.access_plan_version_id \
                          WHERE pg.project_id = $3 AND pg.status = 'active' \
                            AND av.access_plan_id = g.access_plan_id \
                            AND (pg.valid_from IS NULL OR pg.valid_from <= $4) \
                            AND (pg.valid_until IS NULL OR pg.valid_until > $5))\
             ) ORDER BY LOWER(g.name), g.id",
        )
        .bind(project_id)
        .bind(project_id)
        .bind(project_id)
        .bind(&now)
        .bind(&now)
        .fetch_all(&self.pool)
        .await
        .map_err(simple_group_storage)?;

        let effective = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &self.pool, project_id,
        )
        .await
        .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let mut result = Vec::with_capacity(group_ids.len());
        for group_id in group_ids {
            let group = self.load_native_simple_group(&group_id).await?;
            let selected_routes = group
                .route_ids
                .iter()
                .filter_map(|id| id.as_str().parse::<i64>().ok())
                .collect::<BTreeSet<_>>();
            let route_rows = sqlx::query(
                "SELECT m.model_id, r.id AS route_id, d.channel_id \
                 FROM access_plan_versions v \
                 JOIN access_plan_items i ON i.access_plan_version_id = v.id \
                 JOIN models m ON m.id = i.public_model_id \
                 JOIN model_routes r ON r.public_model_id = i.public_model_id \
                 JOIN upstream_model_deployments d ON d.id = r.deployment_id \
                 JOIN channels c ON c.id = d.channel_id \
                 WHERE v.access_plan_id = $1 AND v.status = 'published' \
                   AND v.version = (SELECT MAX(v2.version) FROM access_plan_versions v2 \
                                    WHERE v2.access_plan_id = v.access_plan_id AND v2.status = 'published') \
                   AND m.status = 'enabled' AND m.deleted_at = 0 \
                   AND r.status = 'enabled' AND d.status = 'enabled' \
                   AND c.status = 'enabled' AND c.deleted_at = 0 \
                 ORDER BY m.model_id, d.channel_id, r.id",
            )
            .bind(group.access_plan_id.as_str().parse::<i64>().map_err(|_| {
                SimpleGroupServiceError::Storage("invalid access plan id".into())
            })?)
            .fetch_all(&self.pool)
            .await
            .map_err(simple_group_storage)?;
            let mut models = BTreeSet::new();
            let mut channels = BTreeSet::new();
            for row in route_rows {
                let route_id: i64 = row.get("route_id");
                if !selected_routes.is_empty() && !selected_routes.contains(&route_id) {
                    continue;
                }
                let model: String = row.get("model_id");
                let channel: i64 = row.get("channel_id");
                if effective
                    .routes_by_model
                    .get(&model)
                    .is_some_and(|allowed| allowed.contains(&channel))
                {
                    models.insert(model);
                    channels.insert(channel);
                }
            }
            if models.is_empty() || channels.is_empty() {
                continue;
            }
            result.push(APIKeyAssignableGroup {
                id: group.id,
                name: group.name,
                description: group.description,
                status: group.status,
                allowed_model_ids: models.into_iter().map(ID).collect(),
                allowed_channel_ids: channels.into_iter().map(|id| ID(id.to_string())).collect(),
            });
        }
        Ok(result)
    }

    async fn create_simple_group(
        &self,
        actor_user_id: Option<i64>,
        input: CreateSimpleGroupInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let name = validate_simple_group_name(&input.name)?;
        let access_plan_id = input
            .access_plan_id
            .as_ref()
            .map(|value| parse_simple_group_ref(value.as_str()))
            .transpose()?;
        let model_ids = input.model_ids.map(parse_simple_group_refs).transpose()?;
        let route_ids = input
            .route_ids
            .map(parse_simple_group_refs)
            .transpose()?
            .unwrap_or_default();
        if access_plan_id.is_some() == model_ids.is_some() {
            return Err(SimpleGroupServiceError::Invalid(
                "provide exactly one of accessPlanID or modelIDs".into(),
            ));
        }
        if access_plan_id.is_some() && !route_ids.is_empty() {
            return Err(SimpleGroupServiceError::Invalid(
                "routeIDs belong to the linked Access Plan; omit them when accessPlanID is provided"
                    .into(),
            ));
        }
        let price_tier_id = input
            .price_tier_id
            .as_ref()
            .map(|value| parse_simple_group_ref(value.as_str()))
            .transpose()?;
        if price_tier_id.is_some() == input.multiplier_ppm.is_some() {
            return Err(SimpleGroupServiceError::Invalid(
                "provide exactly one of priceTierID or multiplierPpm".into(),
            ));
        }
        if input.multiplier_ppm.is_some_and(|value| value < 0) {
            return Err(SimpleGroupServiceError::Invalid(
                "multiplierPpm cannot be negative".into(),
            ));
        }
        let default_subscription_plan_id = input
            .default_subscription_plan_id
            .as_ref()
            .map(|value| parse_simple_group_ref(value.as_str()))
            .transpose()?;
        let is_default = input.is_default.unwrap_or(false);
        let group_id = Uuid::now_v7().to_string();
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let audit_settings =
            crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(
                &mut tx,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let before_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;

        let duplicate_name =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM simple_groups WHERE name = $1")
                .bind(&name)
                .fetch_one(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
        if duplicate_name != 0 {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "group name {name:?} already exists"
            )));
        }

        let access_plan_id = if let Some(plan_id) = access_plan_id {
            let status =
                sqlx::query_scalar::<_, String>("SELECT status FROM access_plans WHERE id = $1")
                    .bind(plan_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(simple_group_storage)?
                    .ok_or_else(|| {
                        SimpleGroupServiceError::NotFound(format!("access plan {plan_id}"))
                    })?;
            if status != "enabled" {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "access plan {plan_id} is not enabled"
                )));
            }
            plan_id
        } else {
            let model_ids = model_ids.as_deref().ok_or_else(|| {
                SimpleGroupServiceError::Invalid(
                    "modelIDs are required when accessPlanID is omitted".into(),
                )
            })?;
            for model_id in model_ids {
                let exists = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM models \
                     WHERE id = $1 AND status = 'enabled' AND deleted_at = 0",
                )
                .bind(model_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
                if exists == 0 {
                    return Err(SimpleGroupServiceError::NotFound(format!(
                        "enabled public model {model_id}"
                    )));
                }
            }
            validate_enabled_routes(&mut tx, &route_ids, model_ids).await?;
            let plan_name = format!("{name} Access Plan");
            let plan_name_exists =
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM access_plans WHERE name = $1")
                    .bind(&plan_name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(simple_group_storage)?;
            if plan_name_exists != 0 {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "generated access plan name {plan_name:?} already exists; link that plan explicitly"
                )));
            }
            let description = format!("Managed by Simple Group {group_id}");
            let plan_id =
                crate::wiring_postgres_commercialization::create_access_plan_record_postgres(
                    &mut tx,
                    &plan_name,
                    Some(&description),
                    "enabled",
                    false,
                    now,
                )
                .await
                .map_err(simple_group_storage)?;
            crate::wiring_postgres_commercialization::publish_access_plan_version_postgres(
                &mut tx, plan_id, model_ids, &route_ids, now,
            )
            .await
            .map_err(simple_group_storage)?;
            plan_id
        };

        let price_tier_id = if let Some(tier_id) = price_tier_id {
            let status =
                sqlx::query_scalar::<_, String>("SELECT status FROM price_tiers WHERE id = $1")
                    .bind(tier_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(simple_group_storage)?
                    .ok_or_else(|| {
                        SimpleGroupServiceError::NotFound(format!("price tier {tier_id}"))
                    })?;
            if status != "enabled" {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "price tier {tier_id} is not enabled"
                )));
            }
            tier_id
        } else {
            let tier_name = format!("{name} Price Tier");
            let tier_name_exists =
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM price_tiers WHERE name = $1")
                    .bind(&tier_name)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(simple_group_storage)?;
            if tier_name_exists != 0 {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "generated price tier name {tier_name:?} already exists; link that tier explicitly"
                )));
            }
            crate::wiring_postgres_commercialization::create_price_tier_record_postgres(
                &mut tx,
                &tier_name,
                input.multiplier_ppm.ok_or_else(|| {
                    SimpleGroupServiceError::Invalid(
                        "multiplierPpm is required when priceTierID is omitted".into(),
                    )
                })?,
                "enabled",
                false,
                now,
            )
            .await
            .map_err(simple_group_storage)?
        };

        if let Some(plan_id) = default_subscription_plan_id {
            let status = sqlx::query_scalar::<_, String>(
                "SELECT status FROM subscription_plans WHERE id = $1",
            )
            .bind(plan_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(simple_group_storage)?
            .ok_or_else(|| {
                SimpleGroupServiceError::NotFound(format!("subscription plan {plan_id}"))
            })?;
            if status != "enabled" {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "subscription plan {plan_id} is not enabled"
                )));
            }
        }
        if is_default {
            sqlx::query("UPDATE simple_groups SET is_default = FALSE WHERE is_default = TRUE")
                .execute(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
        }
        sqlx::query(
            "INSERT INTO simple_groups \
             (id, name, description, status, is_default, access_plan_id, \
              price_tier_id, default_subscription_plan_id, created_at, updated_at) \
             VALUES ($1, $2, $3, 'enabled', $4, $5, $6, $7, $8, $9)",
        )
        .bind(&group_id)
        .bind(&name)
        .bind(input.description)
        .bind(is_default)
        .bind(access_plan_id)
        .bind(price_tier_id)
        .bind(default_subscription_plan_id)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(simple_group_storage)?;
        if let Some(user_ids) = input.user_ids {
            let user_ids = parse_simple_group_refs(user_ids)?;
            let project_ids = resolve_personal_project_ids(&mut tx, &user_ids).await?;
            replace_simple_group_projects(
                &mut tx,
                &group_id,
                is_default,
                access_plan_id,
                price_tier_id,
                &project_ids,
                &now,
            )
            .await?;
        }
        let after_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        crate::wiring_postgres_commercialization::insert_pricing_audit(
            &mut tx,
            actor_user_id,
            "create_simple_group_commercial_profile",
            "simple_group",
            &group_id,
            Some(before_snapshot),
            Some(after_snapshot),
            &audit_settings,
        )
        .await
        .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(&group_id).await
    }

    async fn update_simple_group(
        &self,
        actor_user_id: Option<i64>,
        input: UpdateSimpleGroupInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let group_id = validate_normalized_simple_group_id(input.group_id.as_str())?.to_owned();
        let route_ids = input.route_ids.map(parse_simple_group_refs).transpose()?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let audit_settings =
            crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(
                &mut tx,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let before_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        let current = sqlx::query(
            "SELECT name, description, status, is_default, access_plan_id, price_tier_id, \
                    default_subscription_plan_id FROM simple_groups WHERE id = $1",
        )
        .bind(&group_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.clone()))?;

        let current_status: String = current.get("status");
        if current_status == "archived" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} is archived"
            )));
        }
        let status = match input.status {
            Some(SimpleGroupStatus::Enabled) => "enabled",
            Some(SimpleGroupStatus::Disabled) => "disabled",
            Some(SimpleGroupStatus::Archived) => {
                return Err(SimpleGroupServiceError::Invalid(
                    "archive groups with deleteSimpleGroup".into(),
                ));
            }
            None => current_status.as_str(),
        };
        let current_is_default = current.get::<bool, _>("is_default");
        let is_default = input.is_default.unwrap_or(current_is_default);
        if current_is_default && !is_default {
            return Err(SimpleGroupServiceError::Invalid(
                "replace the default by setting another simple group as default".into(),
            ));
        }
        if is_default && status != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(
                "the default simple group must remain enabled".into(),
            ));
        }

        let name = match input.name {
            Some(value) => validate_simple_group_name(&value)?,
            None => current.get("name"),
        };
        let duplicate_name = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM simple_groups WHERE name = $1 AND id <> $2",
        )
        .bind(&name)
        .bind(&group_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(simple_group_storage)?;
        if duplicate_name != 0 {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "group name {name:?} already exists"
            )));
        }
        let description = if input.clear_description.unwrap_or(false) {
            None
        } else {
            input
                .description
                .or_else(|| current.get::<Option<String>, _>("description"))
                .filter(|value| !value.trim().is_empty())
        };

        if input.clear_default_subscription_plan.unwrap_or(false)
            && input.default_subscription_plan_id.is_some()
        {
            return Err(SimpleGroupServiceError::Invalid(
                "defaultSubscriptionPlanID conflicts with clearDefaultSubscriptionPlan".into(),
            ));
        }
        let default_subscription_plan_id = if input.clear_default_subscription_plan.unwrap_or(false)
        {
            None
        } else if let Some(value) = input.default_subscription_plan_id {
            let plan_id = parse_simple_group_ref(value.as_str())?;
            let enabled = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM subscription_plans WHERE id = $1 AND status = 'enabled'",
            )
            .bind(plan_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
            if enabled == 0 {
                return Err(SimpleGroupServiceError::NotFound(format!(
                    "enabled subscription plan {plan_id}"
                )));
            }
            Some(plan_id)
        } else {
            current.get("default_subscription_plan_id")
        };

        let access_plan_id: i64 = current.get("access_plan_id");
        let model_ids = input.model_ids.map(parse_simple_group_refs).transpose()?;
        if model_ids.is_some() || route_ids.is_some() {
            let (current_model_ids, current_route_ids) =
                load_published_access_plan_scope(&mut tx, access_plan_id).await?;
            let model_ids = model_ids.as_deref().unwrap_or(&current_model_ids);
            let route_ids = match route_ids.as_deref() {
                Some(route_ids) => route_ids.to_vec(),
                None if model_ids != current_model_ids.as_slice() => {
                    retain_routes_for_models(&mut tx, &current_route_ids, model_ids).await?
                }
                None => current_route_ids,
            };
            for model_id in model_ids {
                let exists = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM models WHERE id = $1 AND status = 'enabled' AND deleted_at = 0",
                )
                .bind(model_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
                if exists == 0 {
                    return Err(SimpleGroupServiceError::NotFound(format!(
                        "enabled public model {model_id}"
                    )));
                }
            }
            validate_enabled_routes(&mut tx, &route_ids, model_ids).await?;
            let version_id =
                crate::wiring_postgres_commercialization::publish_access_plan_version_postgres(
                    &mut tx,
                    access_plan_id,
                    model_ids,
                    &route_ids,
                    now,
                )
                .await
                .map_err(simple_group_storage)?;
            rebind_active_access_grants(&mut tx, access_plan_id, version_id, &now).await?;
            sqlx::query("UPDATE access_plans SET updated_at = $1 WHERE id = $2")
                .bind(&now)
                .bind(access_plan_id)
                .execute(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
        }

        let price_tier_id: i64 = current.get("price_tier_id");
        if let Some(multiplier_ppm) = input.multiplier_ppm {
            if multiplier_ppm < 0 {
                return Err(SimpleGroupServiceError::Invalid(
                    "multiplierPpm cannot be negative".into(),
                ));
            }
            sqlx::query(
                "UPDATE price_tiers SET multiplier_ppm = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(multiplier_ppm)
            .bind(&now)
            .bind(price_tier_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        }

        if is_default {
            sqlx::query("UPDATE simple_groups SET is_default = FALSE WHERE id <> $1")
                .bind(&group_id)
                .execute(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
        }
        sqlx::query(
            "UPDATE simple_groups SET name = $1, description = $2, status = $3, is_default = $4, \
             default_subscription_plan_id = $5, updated_at = $6 WHERE id = $7",
        )
        .bind(name)
        .bind(description)
        .bind(status)
        .bind(is_default)
        .bind(default_subscription_plan_id)
        .bind(&now)
        .bind(&group_id)
        .execute(&mut *tx)
        .await
        .map_err(simple_group_storage)?;

        if let Some(user_ids) = input.user_ids {
            if status != "enabled" {
                return Err(SimpleGroupServiceError::Invalid(
                    "members cannot be replaced while the simple group is disabled".into(),
                ));
            }
            let user_ids = parse_simple_group_refs(user_ids)?;
            let project_ids = resolve_personal_project_ids(&mut tx, &user_ids).await?;
            replace_simple_group_projects(
                &mut tx,
                &group_id,
                is_default,
                access_plan_id,
                price_tier_id,
                &project_ids,
                &now,
            )
            .await?;
        }
        if status != current_status {
            sync_simple_group_project_profiles(
                &mut tx,
                &group_id,
                (status == "enabled").then_some((access_plan_id, price_tier_id)),
                &now,
            )
            .await?;
        }
        let after_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        crate::wiring_postgres_commercialization::insert_pricing_audit(
            &mut tx,
            actor_user_id,
            "update_simple_group_commercial_profile",
            "simple_group",
            &group_id,
            Some(before_snapshot),
            Some(after_snapshot),
            &audit_settings,
        )
        .await
        .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(&group_id).await
    }

    async fn assign_simple_group_users(
        &self,
        actor_user_id: Option<i64>,
        input: AssignSimpleGroupUsersInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let group_id = validate_normalized_simple_group_id(input.group_id.as_str())?;
        let user_ids = parse_simple_group_refs(input.user_ids)?;
        if user_ids.is_empty() {
            return Err(SimpleGroupServiceError::Invalid(
                "userIDs must contain at least one user".into(),
            ));
        }

        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let audit_settings =
            crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(
                &mut tx,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let before_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        let bundle = sqlx::query(
            "SELECT g.status AS group_status, g.access_plan_id, g.price_tier_id, \
                    a.status AS access_plan_status, t.status AS price_tier_status \
             FROM simple_groups g \
             JOIN access_plans a ON a.id = g.access_plan_id \
             JOIN price_tiers t ON t.id = g.price_tier_id \
             WHERE g.id = $1",
        )
        .bind(group_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.to_owned()))?;
        if bundle.get::<String, _>("group_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} is not enabled"
            )));
        }
        if bundle.get::<String, _>("access_plan_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} access plan is not enabled"
            )));
        }
        if bundle.get::<String, _>("price_tier_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} price tier is not enabled"
            )));
        }
        let access_plan_id: i64 = bundle.get("access_plan_id");
        let price_tier_id: i64 = bundle.get("price_tier_id");

        // Resolve the entire batch before writing anything. This prevents the
        // first valid user from moving when a later user is ambiguous.
        let mut project_ids = BTreeSet::new();
        for user_id in user_ids {
            let candidates = sqlx::query_scalar::<_, i64>(
                "SELECT p.id FROM user_projects up \
                 JOIN users u ON u.id = up.user_id \
                 JOIN projects p ON p.id = up.project_id \
                 JOIN project_commercial_profiles cp ON cp.project_id = p.id \
                 WHERE up.user_id = $1 AND up.is_owner = TRUE \
                   AND u.status = 'activated' AND u.deleted_at = 0 \
                   AND p.status = 'active' AND p.deleted_at = 0 \
                   AND cp.account_type = 'personal' AND cp.status = 'active' \
                 ORDER BY p.id",
            )
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
            if candidates.len() != 1 {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "user {user_id} must resolve to exactly one active owned personal Project; found {}",
                    candidates.len()
                )));
            }
            project_ids.insert(candidates[0]);
        }

        for project_id in project_ids {
            let updated = sqlx::query(
                "UPDATE project_commercial_profiles \
                 SET base_access_plan_id = $1, base_price_tier_id = $2, updated_at = $3 \
                 WHERE project_id = $4 AND account_type = 'personal' AND status = 'active'",
            )
            .bind(access_plan_id)
            .bind(price_tier_id)
            .bind(&now)
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
            if updated.rows_affected() != 1 {
                return Err(SimpleGroupServiceError::Invalid(format!(
                    "project {project_id} no longer has an active personal commercial profile"
                )));
            }
            sqlx::query("DELETE FROM simple_group_projects WHERE project_id = $1")
                .bind(project_id)
                .execute(&mut *tx)
                .await
                .map_err(simple_group_storage)?;
            sqlx::query(
                "INSERT INTO simple_group_projects (simple_group_id, project_id, created_at) \
                 VALUES ($1, $2, $3)",
            )
            .bind(group_id)
            .bind(project_id)
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        }
        sqlx::query("UPDATE simple_groups SET updated_at = $1 WHERE id = $2")
            .bind(&now)
            .bind(group_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        let after_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        crate::wiring_postgres_commercialization::insert_pricing_audit(
            &mut tx,
            actor_user_id,
            "assign_simple_group_projects",
            "simple_group",
            group_id,
            Some(before_snapshot),
            Some(after_snapshot),
            &audit_settings,
        )
        .await
        .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(group_id).await
    }

    async fn update_simple_group_models(
        &self,
        input: UpdateSimpleGroupModelsInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let group_id = validate_normalized_simple_group_id(input.group_id.as_str())?;
        let model_ids = parse_simple_group_refs(input.model_ids)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let row = sqlx::query(
            "SELECT g.status AS group_status, g.access_plan_id, a.status AS access_plan_status \
             FROM simple_groups g JOIN access_plans a ON a.id = g.access_plan_id \
             WHERE g.id = $1",
        )
        .bind(group_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.to_owned()))?;
        if row.get::<String, _>("group_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} is not enabled"
            )));
        }
        if row.get::<String, _>("access_plan_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} access plan is not enabled"
            )));
        }
        for model_id in &model_ids {
            let exists = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM models \
                 WHERE id = $1 AND status = 'enabled' AND deleted_at = 0",
            )
            .bind(model_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
            if exists == 0 {
                return Err(SimpleGroupServiceError::NotFound(format!(
                    "enabled public model {model_id}"
                )));
            }
        }
        let access_plan_id: i64 = row.get("access_plan_id");
        let (_, route_ids) = load_published_access_plan_scope(&mut tx, access_plan_id).await?;
        let route_ids = retain_routes_for_models(&mut tx, &route_ids, &model_ids).await?;
        validate_enabled_routes(&mut tx, &route_ids, &model_ids).await?;
        let version_id =
            crate::wiring_postgres_commercialization::publish_access_plan_version_postgres(
                &mut tx,
                access_plan_id,
                &model_ids,
                &route_ids,
                now,
            )
            .await
            .map_err(simple_group_storage)?;
        rebind_active_access_grants(&mut tx, access_plan_id, version_id, &now).await?;
        sqlx::query("UPDATE access_plans SET updated_at = $1 WHERE id = $2")
            .bind(&now)
            .bind(access_plan_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        sqlx::query("UPDATE simple_groups SET updated_at = $1 WHERE id = $2")
            .bind(&now)
            .bind(group_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(group_id).await
    }

    async fn update_simple_group_price(
        &self,
        actor_user_id: Option<i64>,
        input: UpdateSimpleGroupPriceInput,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let group_id = validate_normalized_simple_group_id(input.group_id.as_str())?;
        if input.multiplier_ppm < 0 {
            return Err(SimpleGroupServiceError::Invalid(
                "multiplierPpm cannot be negative".into(),
            ));
        }
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let audit_settings =
            crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(
                &mut tx,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let row = sqlx::query(
            "SELECT g.status AS group_status, g.price_tier_id, t.status AS price_tier_status \
             FROM simple_groups g JOIN price_tiers t ON t.id = g.price_tier_id \
             WHERE g.id = $1",
        )
        .bind(group_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.to_owned()))?;
        if row.get::<String, _>("group_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} is not enabled"
            )));
        }
        if row.get::<String, _>("price_tier_status") != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "simple group {group_id} price tier is not enabled"
            )));
        }
        let price_tier_id: i64 = row.get("price_tier_id");
        let before_snapshot = sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
            "SELECT to_jsonb(t) FROM price_tiers t WHERE t.id=$1",
        )
        .bind(price_tier_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .0;
        sqlx::query("UPDATE price_tiers SET multiplier_ppm = $1, updated_at = $2 WHERE id = $3")
            .bind(input.multiplier_ppm)
            .bind(&now)
            .bind(price_tier_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        sqlx::query("UPDATE simple_groups SET updated_at = $1 WHERE id = $2")
            .bind(&now)
            .bind(group_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
        let after_snapshot = sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
            "SELECT to_jsonb(t) FROM price_tiers t WHERE t.id=$1",
        )
        .bind(price_tier_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(simple_group_storage)?
        .0;
        crate::wiring_postgres_commercialization::insert_pricing_audit(
            &mut tx,
            actor_user_id,
            "update_simple_group_price",
            "price_tier",
            &price_tier_id.to_string(),
            Some(before_snapshot),
            Some(after_snapshot),
            &audit_settings,
        )
        .await
        .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(group_id).await
    }

    async fn delete_simple_group(
        &self,
        actor_user_id: Option<i64>,
        group_id: &str,
    ) -> Result<SimpleGroup, SimpleGroupServiceError> {
        let group_id = validate_normalized_simple_group_id(group_id)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await.map_err(simple_group_storage)?;
        crate::wiring::lock_accounting_currency_price_writes(&mut tx)
            .await
            .map_err(simple_group_storage)?;
        lock_simple_group_mutations(&mut tx).await?;
        let audit_settings =
            crate::wiring_postgres_commercialization::PgCommercializationAdapter::pricing_audit_settings(
                &mut tx,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        let before_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
        let row = sqlx::query("SELECT status, is_default FROM simple_groups WHERE id = $1")
            .bind(group_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(simple_group_storage)?
            .ok_or_else(|| SimpleGroupServiceError::NotFound(group_id.to_owned()))?;
        if row.get::<bool, _>("is_default") {
            return Err(SimpleGroupServiceError::Invalid(
                "the default simple group cannot be archived; choose another default first".into(),
            ));
        }
        if row.get::<String, _>("status") != "archived" {
            sqlx::query(
                "UPDATE simple_groups SET status = 'archived', is_default = FALSE, updated_at = $1 \
                 WHERE id = $2",
            )
            .bind(&now)
            .bind(group_id)
            .execute(&mut *tx)
            .await
            .map_err(simple_group_storage)?;
            sync_simple_group_project_profiles(&mut tx, group_id, None, &now).await?;
            let after_snapshot = simple_group_pricing_catalog_snapshot(&mut tx).await?;
            crate::wiring_postgres_commercialization::insert_pricing_audit(
                &mut tx,
                actor_user_id,
                "archive_simple_group_commercial_profile",
                "simple_group",
                group_id,
                Some(before_snapshot),
                Some(after_snapshot),
                &audit_settings,
            )
            .await
            .map_err(|error| SimpleGroupServiceError::Storage(error.to_string()))?;
        }
        tx.commit().await.map_err(simple_group_storage)?;
        self.load_native_simple_group(group_id).await
    }
}

async fn simple_group_pricing_catalog_snapshot(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<serde_json::Value, SimpleGroupServiceError> {
    let rows = sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
        "SELECT jsonb_build_object( \
             'group',to_jsonb(g), \
             'priceTier',to_jsonb(t), \
             'projectAssignments',COALESCE(( \
               SELECT jsonb_agg(to_jsonb(gp) ORDER BY gp.project_id) \
               FROM simple_group_projects gp WHERE gp.simple_group_id=g.id \
             ),'[]'::jsonb), \
             'projectCommercialProfiles',COALESCE(( \
               SELECT jsonb_agg(to_jsonb(cp) ORDER BY cp.project_id) \
               FROM project_commercial_profiles cp \
               JOIN simple_group_projects gp ON gp.project_id=cp.project_id \
               WHERE gp.simple_group_id=g.id \
             ),'[]'::jsonb) \
           ) \
         FROM simple_groups g JOIN price_tiers t ON t.id=g.price_tier_id \
         ORDER BY g.id",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    Ok(serde_json::Value::Array(
        rows.into_iter().map(|sqlx::types::Json(row)| row).collect(),
    ))
}

async fn resolve_personal_project_ids(
    tx: &mut Transaction<'_, Postgres>,
    user_ids: &[i64],
) -> Result<BTreeSet<i64>, SimpleGroupServiceError> {
    let mut project_ids = BTreeSet::new();
    for user_id in user_ids {
        let candidates = sqlx::query_scalar::<_, i64>(
            "SELECT p.id FROM user_projects up \
             JOIN users u ON u.id = up.user_id \
             JOIN projects p ON p.id = up.project_id \
             JOIN project_commercial_profiles cp ON cp.project_id = p.id \
             WHERE up.user_id = $1 AND up.is_owner = TRUE \
               AND u.status = 'activated' AND u.deleted_at = 0 \
               AND p.status = 'active' AND p.deleted_at = 0 \
               AND cp.account_type = 'personal' AND cp.status = 'active' \
             ORDER BY p.id",
        )
        .bind(user_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(simple_group_storage)?;
        if candidates.len() != 1 {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "user {user_id} must resolve to exactly one active owned personal Project; found {}",
                candidates.len()
            )));
        }
        project_ids.insert(candidates[0]);
    }
    Ok(project_ids)
}

async fn replace_simple_group_projects(
    tx: &mut Transaction<'_, Postgres>,
    group_id: &str,
    is_default: bool,
    access_plan_id: i64,
    price_tier_id: i64,
    selected_project_ids: &BTreeSet<i64>,
    now: &DateTime<Utc>,
) -> Result<(), SimpleGroupServiceError> {
    let current_project_ids = sqlx::query_scalar::<_, i64>(
        "SELECT project_id FROM simple_group_projects WHERE simple_group_id = $1",
    )
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(simple_group_storage)?
    .into_iter()
    .collect::<BTreeSet<_>>();
    let removed_project_ids = current_project_ids
        .difference(selected_project_ids)
        .copied()
        .collect::<Vec<_>>();

    if is_default && !removed_project_ids.is_empty() {
        return Err(SimpleGroupServiceError::Invalid(
            "default group members cannot be removed directly; assign them to another group".into(),
        ));
    }
    if !removed_project_ids.is_empty() {
        let fallback = sqlx::query(
            "SELECT id, access_plan_id, price_tier_id FROM simple_groups \
             WHERE is_default = TRUE AND status = 'enabled' AND id <> $1",
        )
        .bind(group_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| {
            SimpleGroupServiceError::Invalid(
                "set an enabled default simple group before removing members".into(),
            )
        })?;
        let fallback_id: String = fallback.get("id");
        let fallback_access_plan_id: i64 = fallback.get("access_plan_id");
        let fallback_price_tier_id: i64 = fallback.get("price_tier_id");
        for project_id in removed_project_ids {
            sqlx::query("DELETE FROM simple_group_projects WHERE project_id = $1")
                .bind(project_id)
                .execute(&mut **tx)
                .await
                .map_err(simple_group_storage)?;
            sqlx::query(
                "INSERT INTO simple_group_projects (simple_group_id, project_id, created_at) \
                 VALUES ($1, $2, $3)",
            )
            .bind(&fallback_id)
            .bind(project_id)
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(simple_group_storage)?;
            update_project_profile_bundle(
                tx,
                project_id,
                Some((fallback_access_plan_id, fallback_price_tier_id)),
                now,
            )
            .await?;
        }
        sqlx::query("UPDATE simple_groups SET updated_at = $1 WHERE id = $2")
            .bind(now)
            .bind(fallback_id)
            .execute(&mut **tx)
            .await
            .map_err(simple_group_storage)?;
    }

    for project_id in selected_project_ids {
        sqlx::query("DELETE FROM simple_group_projects WHERE project_id = $1")
            .bind(project_id)
            .execute(&mut **tx)
            .await
            .map_err(simple_group_storage)?;
        sqlx::query(
            "INSERT INTO simple_group_projects (simple_group_id, project_id, created_at) \
             VALUES ($1, $2, $3)",
        )
        .bind(group_id)
        .bind(project_id)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(simple_group_storage)?;
        update_project_profile_bundle(tx, *project_id, Some((access_plan_id, price_tier_id)), now)
            .await?;
    }
    Ok(())
}

async fn sync_simple_group_project_profiles(
    tx: &mut Transaction<'_, Postgres>,
    group_id: &str,
    bundle: Option<(i64, i64)>,
    now: &DateTime<Utc>,
) -> Result<(), SimpleGroupServiceError> {
    let project_ids = sqlx::query_scalar::<_, i64>(
        "SELECT project_id FROM simple_group_projects WHERE simple_group_id = $1",
    )
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    for project_id in project_ids {
        update_project_profile_bundle(tx, project_id, bundle, now).await?;
    }
    Ok(())
}

async fn update_project_profile_bundle(
    tx: &mut Transaction<'_, Postgres>,
    project_id: i64,
    bundle: Option<(i64, i64)>,
    now: &DateTime<Utc>,
) -> Result<(), SimpleGroupServiceError> {
    let (access_plan_id, price_tier_id) = bundle
        .map(|(access_plan_id, price_tier_id)| (Some(access_plan_id), Some(price_tier_id)))
        .unwrap_or((None, None));
    let updated = sqlx::query(
        "UPDATE project_commercial_profiles \
         SET base_access_plan_id = $1, base_price_tier_id = $2, updated_at = $3 \
         WHERE project_id = $4 AND account_type = 'personal' AND status = 'active'",
    )
    .bind(access_plan_id)
    .bind(price_tier_id)
    .bind(now)
    .bind(project_id)
    .execute(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    if updated.rows_affected() != 1 {
        return Err(SimpleGroupServiceError::Invalid(format!(
            "project {project_id} no longer has an active personal commercial profile"
        )));
    }
    Ok(())
}

fn simple_group_storage(error: sqlx::Error) -> SimpleGroupServiceError {
    SimpleGroupServiceError::Storage(error.to_string())
}

fn validate_simple_group_name(value: &str) -> Result<String, SimpleGroupServiceError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 {
        return Err(SimpleGroupServiceError::Invalid(
            "name must contain 1 to 80 characters".into(),
        ));
    }
    Ok(value.to_owned())
}

fn parse_simple_group_ref(value: &str) -> Result<i64, SimpleGroupServiceError> {
    value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .parse()
        .map_err(|_| SimpleGroupServiceError::Invalid(format!("invalid id {value:?}")))
}

fn parse_simple_group_refs(values: Vec<ID>) -> Result<Vec<i64>, SimpleGroupServiceError> {
    let mut values = values
        .into_iter()
        .map(|value| parse_simple_group_ref(value.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

async fn validate_enabled_routes(
    tx: &mut Transaction<'_, Postgres>,
    route_ids: &[i64],
    model_ids: &[i64],
) -> Result<(), SimpleGroupServiceError> {
    let allowed_models = model_ids.iter().copied().collect::<BTreeSet<_>>();
    for route_id in route_ids {
        let route = sqlx::query_as::<_, (String, i64, i64, i64)>(
            "SELECT r.status, r.public_model_id, d.id, d.channel_id FROM model_routes r \
             JOIN upstream_model_deployments d ON d.id = r.deployment_id \
             JOIN channels c ON c.id = d.channel_id \
             WHERE r.id = $1 AND d.status = 'enabled' AND c.status = 'enabled' AND c.deleted_at = 0",
        )
        .bind(route_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(simple_group_storage)?
        .ok_or_else(|| SimpleGroupServiceError::NotFound(format!("model route {route_id}")))?;
        let (status, public_model_id, deployment_id, channel_id) = route;
        if status != "enabled" {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "model route {route_id} is not enabled"
            )));
        }
        if !allowed_models.contains(&public_model_id) {
            return Err(SimpleGroupServiceError::Invalid(format!(
                "model route {route_id} (deployment {deployment_id}, channel {channel_id}) belongs to public model {public_model_id}, which is not in this model group"
            )));
        }
    }
    Ok(())
}

async fn retain_routes_for_models(
    tx: &mut Transaction<'_, Postgres>,
    route_ids: &[i64],
    model_ids: &[i64],
) -> Result<Vec<i64>, SimpleGroupServiceError> {
    let allowed_models = model_ids.iter().copied().collect::<BTreeSet<_>>();
    let mut retained = Vec::new();
    for route_id in route_ids {
        let public_model_id =
            sqlx::query_scalar::<_, i64>("SELECT public_model_id FROM model_routes WHERE id = $1")
                .bind(route_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(simple_group_storage)?
                .ok_or_else(|| {
                    SimpleGroupServiceError::NotFound(format!("model route {route_id}"))
                })?;
        if allowed_models.contains(&public_model_id) {
            retained.push(*route_id);
        }
    }
    Ok(retained)
}

async fn load_published_access_plan_scope(
    tx: &mut Transaction<'_, Postgres>,
    access_plan_id: i64,
) -> Result<(Vec<i64>, Vec<i64>), SimpleGroupServiceError> {
    let version_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM access_plan_versions \
         WHERE access_plan_id = $1 AND status = 'published' ORDER BY version DESC LIMIT 1",
    )
    .bind(access_plan_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(simple_group_storage)?
    .ok_or_else(|| {
        SimpleGroupServiceError::Invalid(format!(
            "access plan {access_plan_id} has no published version"
        ))
    })?;
    let model_ids = sqlx::query_scalar::<_, i64>(
        "SELECT public_model_id FROM access_plan_items \
         WHERE access_plan_version_id = $1 ORDER BY public_model_id",
    )
    .bind(version_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    let route_ids = sqlx::query_scalar::<_, i64>(
        "SELECT model_route_id FROM access_plan_route_items \
         WHERE access_plan_version_id = $1 ORDER BY model_route_id",
    )
    .bind(version_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    Ok((model_ids, route_ids))
}

/// A model group is an administrator-managed live access boundary. Publishing
/// a new version must therefore move every current grant and every renewable
/// subscription snapshot for that access plan to the new version.
/// Updating both in the same transaction prevents a later resume or renewal
/// from restoring the archived version.
async fn rebind_active_access_grants(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    access_plan_id: i64,
    version_id: i64,
    now: &DateTime<Utc>,
) -> Result<(), SimpleGroupServiceError> {
    sqlx::query(
        "UPDATE project_access_grants SET access_plan_version_id = $1, updated_at = $2 \
         WHERE status IN ('active', 'paused') AND access_plan_version_id IN \
         (SELECT id FROM access_plan_versions WHERE access_plan_id = $3 AND id <> $4)",
    )
    .bind(version_id)
    .bind(now)
    .bind(access_plan_id)
    .bind(version_id)
    .execute(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    sqlx::query(
        "UPDATE user_subscription_access_plan_snapshots SET access_plan_version_id = $1 \
         WHERE access_plan_id = $2 AND access_plan_version_id <> $3 \
         AND subscription_id IN \
         (SELECT id FROM user_subscriptions WHERE status IN ('active', 'paused', 'expired'))",
    )
    .bind(version_id)
    .bind(access_plan_id)
    .bind(version_id)
    .execute(&mut **tx)
    .await
    .map_err(simple_group_storage)?;
    Ok(())
}

fn validate_normalized_simple_group_id(value: &str) -> Result<&str, SimpleGroupServiceError> {
    Uuid::parse_str(value).map_err(|_| {
        SimpleGroupServiceError::Invalid(format!(
            "groupID {value:?} is not a normalized Simple Group ID"
        ))
    })?;
    Ok(value)
}

fn simple_group_from_native_row(
    row: PgRow,
    member_project_ids: Vec<i64>,
    member_user_ids: Vec<i64>,
    unresolved_member_count: usize,
    model_ids: Vec<i64>,
    route_ids: Vec<i64>,
) -> Result<SimpleGroup, SimpleGroupServiceError> {
    let created_at = row.get::<DateTime<Utc>, _>("created_at");
    let updated_at = row.get::<DateTime<Utc>, _>("updated_at");
    let status = match row.get::<String, _>("status").as_str() {
        "disabled" => SimpleGroupStatus::Disabled,
        "archived" => SimpleGroupStatus::Archived,
        _ => SimpleGroupStatus::Enabled,
    };
    Ok(SimpleGroup {
        id: ID(row.get("id")),
        name: row.get("name"),
        description: row.get("description"),
        status,
        is_default: row.get::<bool, _>("is_default"),
        access_plan_id: ID(row.get::<i64, _>("access_plan_id").to_string()),
        price_tier_id: ID(row.get::<i64, _>("price_tier_id").to_string()),
        default_subscription_plan_id: row
            .get::<Option<i64>, _>("default_subscription_plan_id")
            .map(|value| ID(value.to_string())),
        model_ids: model_ids
            .into_iter()
            .map(|value| node_id("Model", value))
            .collect(),
        route_ids: route_ids
            .into_iter()
            .map(|value| ID(value.to_string()))
            .collect(),
        multiplier_ppm: row.get("multiplier_ppm"),
        member_user_ids: member_user_ids
            .into_iter()
            .map(|value| node_id("User", value))
            .collect(),
        member_project_ids: member_project_ids
            .into_iter()
            .map(|value| node_id("Project", value))
            .collect(),
        unresolved_member_count: i32::try_from(unresolved_member_count).map_err(|_| {
            SimpleGroupServiceError::Storage("unresolved member count exceeds i32".into())
        })?,
        created_at: TimeScalar(created_at),
        updated_at: TimeScalar(updated_at),
    })
}

fn node_id(kind: &str, value: i64) -> ID {
    ID(format!("gid://conduit/{kind}/{value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_input(
        name: String,
        model_ids: Vec<i64>,
        route_ids: Vec<i64>,
        multiplier_ppm: i64,
        is_default: bool,
        default_subscription_plan_id: Option<i64>,
        user_ids: Option<Vec<i64>>,
    ) -> CreateSimpleGroupInput {
        CreateSimpleGroupInput {
            name,
            description: Some("PostgreSQL model-group integration".to_string()),
            is_default: Some(is_default),
            access_plan_id: None,
            model_ids: Some(model_ids.into_iter().map(|id| ID(id.to_string())).collect()),
            route_ids: Some(route_ids.into_iter().map(|id| ID(id.to_string())).collect()),
            price_tier_id: None,
            multiplier_ppm: Some(multiplier_ppm),
            default_subscription_plan_id: default_subscription_plan_id.map(|id| ID(id.to_string())),
            user_ids: user_ids.map(|ids| ids.into_iter().map(|id| node_id("User", id)).collect()),
        }
    }

    fn update_input(group_id: ID) -> UpdateSimpleGroupInput {
        UpdateSimpleGroupInput {
            group_id,
            name: None,
            description: None,
            clear_description: None,
            status: None,
            is_default: None,
            model_ids: None,
            route_ids: None,
            multiplier_ppm: None,
            default_subscription_plan_id: None,
            clear_default_subscription_plan: None,
            user_ids: None,
        }
    }

    #[tokio::test]
    async fn postgres_model_group_rebind_updates_renewable_subscription_snapshots()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        let mut tx = pool.begin().await?;
        sqlx::raw_sql(
            "CREATE TEMP TABLE access_plan_versions (
                id BIGINT PRIMARY KEY, access_plan_id BIGINT NOT NULL
             ) ON COMMIT DROP;
             CREATE TEMP TABLE project_access_grants (
                id BIGINT PRIMARY KEY, access_plan_version_id BIGINT NOT NULL,
                status TEXT NOT NULL, updated_at TIMESTAMPTZ NOT NULL
             ) ON COMMIT DROP;
             CREATE TEMP TABLE user_subscriptions (
                id BIGINT PRIMARY KEY, status TEXT NOT NULL
             ) ON COMMIT DROP;
             CREATE TEMP TABLE user_subscription_access_plan_snapshots (
                subscription_id BIGINT NOT NULL, access_plan_id BIGINT NOT NULL,
                access_plan_version_id BIGINT NOT NULL,
                PRIMARY KEY (subscription_id, access_plan_id)
             ) ON COMMIT DROP;
             INSERT INTO access_plan_versions (id, access_plan_id) VALUES
                (100, 10), (101, 10), (110, 11);
             INSERT INTO user_subscriptions (id, status) VALUES
                (1, 'active'), (2, 'paused'), (3, 'expired'), (4, 'canceled'), (5, 'active');
             INSERT INTO user_subscription_access_plan_snapshots
                (subscription_id, access_plan_id, access_plan_version_id) VALUES
                (1, 10, 100), (2, 10, 100), (3, 10, 100), (4, 10, 100), (5, 11, 110);
             INSERT INTO project_access_grants (id, access_plan_version_id, status, updated_at) VALUES
                (1, 100, 'active', now()), (2, 100, 'paused', now()),
                (3, 100, 'expired', now()), (4, 100, 'canceled', now()),
                (5, 110, 'active', now());",
        )
        .execute(&mut *tx)
        .await?;

        rebind_active_access_grants(&mut tx, 10, 101, &Utc::now()).await?;

        let snapshots = sqlx::query(
            "SELECT subscription_id, access_plan_version_id
             FROM user_subscription_access_plan_snapshots ORDER BY subscription_id",
        )
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|row| {
            (
                row.get::<i64, _>("subscription_id"),
                row.get::<i64, _>("access_plan_version_id"),
            )
        })
        .collect::<Vec<_>>();
        assert_eq!(
            snapshots,
            vec![(1, 101), (2, 101), (3, 101), (4, 100), (5, 110)]
        );

        let grants =
            sqlx::query("SELECT id, access_plan_version_id FROM project_access_grants ORDER BY id")
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<i64, _>("id"),
                        row.get::<i64, _>("access_plan_version_id"),
                    )
                })
                .collect::<Vec<_>>();
        assert_eq!(
            grants,
            vec![(1, 101), (2, 101), (3, 100), (4, 100), (5, 110)]
        );
        tx.rollback().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_model_groups_preserve_routes_membership_and_api_key_visibility()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let adapter = PgSimpleGroupAdapter::new(pool.clone());
        let suffix = Uuid::new_v4().simple().to_string();

        let channel_one = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\", name, status, credentials, supported_models, default_test_model) \
             VALUES ('openai', $1, 'enabled', '{}'::jsonb, '[]'::jsonb, '') RETURNING id",
        )
        .bind(format!("PG Group C1 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let channel_two = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\", name, status, credentials, supported_models, default_test_model) \
             VALUES ('openai', $1, 'enabled', '{}'::jsonb, '[]'::jsonb, '') RETURNING id",
        )
        .bind(format!("PG Group C2 {suffix}"))
        .fetch_one(&pool)
        .await?;
        let public_model_one = sqlx::query_scalar::<_, i64>(
            "INSERT INTO models \
             (developer, model_id, \"type\", name, icon, \"group\", model_card, settings, status) \
             VALUES ('test', $1, 'chat', $2, '', 'test', '{}'::jsonb, '{}'::jsonb, 'enabled') \
             RETURNING id",
        )
        .bind(format!("pg-group-model-one-{suffix}"))
        .bind(format!("PG Group Model One {suffix}"))
        .fetch_one(&pool)
        .await?;
        let public_model_two = sqlx::query_scalar::<_, i64>(
            "INSERT INTO models \
             (developer, model_id, \"type\", name, icon, \"group\", model_card, settings, status) \
             VALUES ('test', $1, 'chat', $2, '', 'test', '{}'::jsonb, '{}'::jsonb, 'enabled') \
             RETURNING id",
        )
        .bind(format!("pg-group-model-two-{suffix}"))
        .bind(format!("PG Group Model Two {suffix}"))
        .fetch_one(&pool)
        .await?;

        // The first two deployments intentionally expose the exact same
        // upstream model name. Their Route IDs retain the channel identity.
        let deployment_one = sqlx::query_scalar::<_, i64>(
            "INSERT INTO upstream_model_deployments \
             (channel_id, upstream_model_id, internal_name, variant, status, source) \
             VALUES ($1, $2, 'M1-C1', '', 'enabled', 'manual') RETURNING id",
        )
        .bind(channel_one)
        .bind(format!("same-upstream-name-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let deployment_two = sqlx::query_scalar::<_, i64>(
            "INSERT INTO upstream_model_deployments \
             (channel_id, upstream_model_id, internal_name, variant, status, source) \
             VALUES ($1, $2, 'M1-C2', '', 'enabled', 'manual') RETURNING id",
        )
        .bind(channel_two)
        .bind(format!("same-upstream-name-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let deployment_three = sqlx::query_scalar::<_, i64>(
            "INSERT INTO upstream_model_deployments \
             (channel_id, upstream_model_id, internal_name, variant, status, source) \
             VALUES ($1, $2, 'M2-C2', '', 'enabled', 'manual') RETURNING id",
        )
        .bind(channel_two)
        .bind(format!("second-upstream-name-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let route_one = sqlx::query_scalar::<_, i64>(
            "INSERT INTO model_routes (public_model_id, deployment_id, status) \
             VALUES ($1, $2, 'enabled') RETURNING id",
        )
        .bind(public_model_one)
        .bind(deployment_one)
        .fetch_one(&pool)
        .await?;
        let route_two = sqlx::query_scalar::<_, i64>(
            "INSERT INTO model_routes (public_model_id, deployment_id, status) \
             VALUES ($1, $2, 'enabled') RETURNING id",
        )
        .bind(public_model_one)
        .bind(deployment_two)
        .fetch_one(&pool)
        .await?;
        let route_three = sqlx::query_scalar::<_, i64>(
            "INSERT INTO model_routes (public_model_id, deployment_id, status) \
             VALUES ($1, $2, 'enabled') RETURNING id",
        )
        .bind(public_model_two)
        .bind(deployment_three)
        .fetch_one(&pool)
        .await?;

        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users (email, password, status) \
             VALUES ($1, 'unused', 'activated') RETURNING id",
        )
        .bind(format!("pg-group-{suffix}@example.com"))
        .fetch_one(&pool)
        .await?;
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, status, profiles) \
             VALUES ($1, 'active', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Group Project {suffix}"))
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_projects (user_id, project_id, is_owner) VALUES ($1, $2, TRUE)",
        )
        .bind(user_id)
        .bind(project_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_commercial_profiles \
             (project_id, account_type, billing_currency, status, created_at, updated_at) \
             VALUES ($1, 'personal', 'STATION_CREDIT', 'active', now(), now())",
        )
        .bind(project_id)
        .execute(&pool)
        .await?;
        let subscription_plan_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO subscription_plans \
             (name, interval_unit, status, created_at, updated_at) \
             VALUES ($1, 'month', 'enabled', now(), now()) RETURNING id",
        )
        .bind(format!("PG Group Subscription {suffix}"))
        .fetch_one(&pool)
        .await?;

        let first = adapter
            .create_simple_group(
                None,
                create_input(
                    format!("PG Group First {suffix}"),
                    vec![public_model_one, public_model_two],
                    vec![route_one, route_three],
                    1_200_000,
                    true,
                    Some(subscription_plan_id),
                    Some(vec![user_id]),
                ),
            )
            .await?;
        assert_eq!(
            first.model_ids,
            vec![
                node_id("Model", public_model_one),
                node_id("Model", public_model_two)
            ]
        );
        assert_eq!(
            first.route_ids,
            vec![ID(route_one.to_string()), ID(route_three.to_string())]
        );
        assert_eq!(first.member_user_ids, vec![node_id("User", user_id)]);
        assert_eq!(
            first.member_project_ids,
            vec![node_id("Project", project_id)]
        );
        assert_eq!(
            first.default_subscription_plan_id,
            Some(ID(subscription_plan_id.to_string()))
        );

        let model_one_key = format!("pg-group-model-one-{suffix}");
        let model_two_key = format!("pg-group-model-two-{suffix}");
        let access = crate::wiring_project_access::resolve_effective_project_access_postgres(
            &pool, project_id,
        )
        .await?;
        assert_eq!(
            access.routes_by_model.get(&model_one_key),
            Some(&BTreeSet::from([channel_one]))
        );
        assert_eq!(
            access.routes_by_model.get(&model_two_key),
            Some(&BTreeSet::from([channel_two]))
        );
        let assignable = adapter.api_key_assignable_groups(project_id).await?;
        assert_eq!(assignable.len(), 1);
        assert!(
            assignable[0]
                .allowed_model_ids
                .contains(&ID(model_one_key.clone()))
        );
        assert!(
            assignable[0]
                .allowed_model_ids
                .contains(&ID(model_two_key.clone()))
        );
        let unrelated_project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects (name, status, profiles) \
             VALUES ($1, 'active', '{}'::jsonb) RETURNING id",
        )
        .bind(format!("PG Unrelated Group Project {suffix}"))
        .fetch_one(&pool)
        .await?;
        assert!(
            adapter
                .api_key_assignable_groups(unrelated_project_id)
                .await?
                .is_empty(),
            "a Project must not see another Project's assignable model group"
        );

        let access_plan_id = parse_simple_group_ref(first.access_plan_id.as_str())?;
        let original_version_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM access_plan_versions \
             WHERE access_plan_id = $1 AND status = 'published'",
        )
        .bind(access_plan_id)
        .fetch_one(&pool)
        .await?;
        let grant_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO project_access_grants \
             (project_id, access_plan_version_id, source_type, source_id, status, created_at, updated_at) \
             VALUES ($1, $2, 'test', $3, 'active', now(), now()) RETURNING id",
        )
        .bind(project_id)
        .bind(original_version_id)
        .bind(format!("pg-group-{suffix}"))
        .fetch_one(&pool)
        .await?;

        let model_updated = adapter
            .update_simple_group_models(UpdateSimpleGroupModelsInput {
                group_id: first.id.clone(),
                model_ids: vec![ID(public_model_one.to_string())],
            })
            .await?;
        assert_eq!(
            model_updated.model_ids,
            vec![node_id("Model", public_model_one)]
        );
        assert_eq!(model_updated.route_ids, vec![ID(route_one.to_string())]);
        let rebound_version_id = sqlx::query_scalar::<_, i64>(
            "SELECT access_plan_version_id FROM project_access_grants WHERE id = $1",
        )
        .bind(grant_id)
        .fetch_one(&pool)
        .await?;
        assert_ne!(rebound_version_id, original_version_id);

        let switched = adapter
            .update_simple_group(
                None,
                UpdateSimpleGroupInput {
                    route_ids: Some(vec![ID(route_two.to_string())]),
                    ..update_input(first.id.clone())
                },
            )
            .await?;
        assert_eq!(switched.route_ids, vec![ID(route_two.to_string())]);
        let switched_access =
            crate::wiring_project_access::resolve_effective_project_access_postgres(
                &pool, project_id,
            )
            .await?;
        assert_eq!(
            switched_access.routes_by_model.get(&model_one_key),
            Some(&BTreeSet::from([channel_two]))
        );

        let invalid_route = adapter
            .update_simple_group(
                None,
                UpdateSimpleGroupInput {
                    route_ids: Some(vec![ID(route_three.to_string())]),
                    ..update_input(first.id.clone())
                },
            )
            .await;
        assert!(matches!(
            invalid_route,
            Err(SimpleGroupServiceError::Invalid(message))
                if message.contains("belongs to public model")
                    && message.contains("deployment")
                    && message.contains("channel")
        ));
        assert_eq!(
            adapter
                .load_native_simple_group(first.id.as_str())
                .await?
                .route_ids,
            vec![ID(route_two.to_string())]
        );
        assert_eq!(
            adapter
                .update_simple_group_price(
                    Some(55),
                    UpdateSimpleGroupPriceInput {
                        group_id: first.id.clone(),
                        multiplier_ppm: 1_500_000,
                    }
                )
                .await?
                .multiplier_ppm,
            1_500_000
        );
        let price_audit = sqlx::query(
            "SELECT actor_id,before_snapshot,after_snapshot FROM pricing_change_audits \
             WHERE operation='update_simple_group_price'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(price_audit.get::<Option<i64>, _>("actor_id"), Some(55));
        assert_eq!(
            price_audit
                .get::<sqlx::types::Json<serde_json::Value>, _>("before_snapshot")
                .0["multiplier_ppm"],
            1_200_000
        );
        assert_eq!(
            price_audit
                .get::<sqlx::types::Json<serde_json::Value>, _>("after_snapshot")
                .0["multiplier_ppm"],
            1_500_000
        );

        let second = adapter
            .create_simple_group(
                None,
                create_input(
                    format!("PG Group Second {suffix}"),
                    vec![public_model_two],
                    vec![route_three],
                    900_000,
                    true,
                    None,
                    None,
                ),
            )
            .await?;
        let moved = adapter
            .update_simple_group(
                None,
                UpdateSimpleGroupInput {
                    user_ids: Some(Vec::new()),
                    ..update_input(first.id.clone())
                },
            )
            .await?;
        assert!(moved.member_project_ids.is_empty());
        assert_eq!(
            adapter
                .load_native_simple_group(second.id.as_str())
                .await?
                .member_project_ids,
            vec![node_id("Project", project_id)]
        );
        adapter
            .assign_simple_group_users(
                None,
                AssignSimpleGroupUsersInput {
                    group_id: first.id.clone(),
                    user_ids: vec![node_id("User", user_id)],
                },
            )
            .await?;
        let archived = adapter.delete_simple_group(None, first.id.as_str()).await?;
        assert_eq!(archived.status, SimpleGroupStatus::Archived);
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT base_access_plan_id FROM project_commercial_profiles WHERE project_id = $1",
            )
            .bind(project_id)
            .fetch_one(&pool)
            .await?,
            None
        );
        assert!(matches!(
            adapter.delete_simple_group(None, second.id.as_str()).await,
            Err(SimpleGroupServiceError::Invalid(message)) if message.contains("default")
        ));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM simple_groups WHERE is_default = TRUE",
            )
            .fetch_one(&pool)
            .await?,
            1
        );

        let first_price_id = parse_simple_group_ref(first.price_tier_id.as_str())?;
        let second_price_id = parse_simple_group_ref(second.price_tier_id.as_str())?;
        let second_access_id = parse_simple_group_ref(second.access_plan_id.as_str())?;
        sqlx::query("DELETE FROM simple_group_projects WHERE project_id = $1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM simple_groups WHERE id = $1 OR id = $2")
            .bind(first.id.as_str())
            .bind(second.id.as_str())
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM project_access_grants WHERE id = $1")
            .bind(grant_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "DELETE FROM access_plan_route_items WHERE access_plan_version_id IN \
             (SELECT id FROM access_plan_versions WHERE access_plan_id = $1 OR access_plan_id = $2)",
        )
        .bind(access_plan_id)
        .bind(second_access_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "DELETE FROM access_plan_items WHERE access_plan_version_id IN \
             (SELECT id FROM access_plan_versions WHERE access_plan_id = $1 OR access_plan_id = $2)",
        )
        .bind(access_plan_id)
        .bind(second_access_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "DELETE FROM access_plan_versions WHERE access_plan_id = $1 OR access_plan_id = $2",
        )
        .bind(access_plan_id)
        .bind(second_access_id)
        .execute(&pool)
        .await?;
        sqlx::query("DELETE FROM access_plans WHERE id = $1 OR id = $2")
            .bind(access_plan_id)
            .bind(second_access_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM price_tiers WHERE id = $1 OR id = $2")
            .bind(first_price_id)
            .bind(second_price_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM subscription_plans WHERE id = $1")
            .bind(subscription_plan_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM project_commercial_profiles WHERE project_id = $1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM user_projects WHERE project_id = $1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(unrelated_project_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM model_routes WHERE id = $1 OR id = $2 OR id = $3")
            .bind(route_one)
            .bind(route_two)
            .bind(route_three)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM upstream_model_deployments WHERE id = $1 OR id = $2 OR id = $3")
            .bind(deployment_one)
            .bind(deployment_two)
            .bind(deployment_three)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM models WHERE id = $1 OR id = $2")
            .bind(public_model_one)
            .bind(public_model_two)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM channels WHERE id = $1 OR id = $2")
            .bind(channel_one)
            .bind(channel_two)
            .execute(&pool)
            .await?;
        Ok(())
    }
}

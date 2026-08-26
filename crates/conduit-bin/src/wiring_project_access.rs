use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use conduit_admin_graphql::model_catalog::ModelCatalogError;
use conduit_services::project_commercialization::{
    MULTIPLIER_ONE_PPM, ProjectPriceAdjustment, resolve_project_price_multiplier,
};
use rust_decimal::Decimal;
use sqlx::PgPool;
use sqlx::Row;

/// The single request-time customer access result.
///
/// Routes are retained per public model so combining multiple entitlement
/// sources never creates an accidental model/channel cross product.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectiveProjectAccess {
    pub routes_by_model: BTreeMap<String, BTreeSet<i64>>,
    /// Canonical model -> channel -> concrete provider model selected from the
    /// enabled Route mapping for that public model and channel.
    pub upstream_models_by_model: BTreeMap<String, BTreeMap<i64, String>>,
    pub price_multiplier: Decimal,
}

impl EffectiveProjectAccess {
    pub fn channels_for_model(&self, model: &str) -> BTreeSet<i64> {
        self.routes_by_model.get(model).cloned().unwrap_or_default()
    }

    pub fn upstream_models_for_model(&self, model: &str) -> BTreeMap<i64, String> {
        self.upstream_models_by_model
            .get(model)
            .cloned()
            .unwrap_or_default()
    }
}

fn query(error: sqlx::Error) -> ModelCatalogError {
    ModelCatalogError::Query(error.to_string())
}

pub async fn resolve_effective_project_access_postgres(
    pool: &PgPool,
    project_id: i64,
) -> Result<EffectiveProjectAccess, ModelCatalogError> {
    let now = Utc::now();
    let mut versions = Vec::new();
    if let Some(plan_id) = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT base_access_plan_id FROM project_commercial_profiles WHERE project_id=$1 AND status='active'",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(query)?
    .flatten()
    {
        if let Some(id) = sqlx::query_scalar::<_, i64>(
            "SELECT v.id FROM access_plans p JOIN access_plan_versions v ON v.access_plan_id=p.id \
             WHERE p.id=$1 AND p.status='enabled' AND v.status='published' \
             AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
             AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) ORDER BY v.version DESC LIMIT 1",
        )
        .bind(plan_id)
        .bind(now)
        .fetch_optional(pool)
        .await
        .map_err(query)?
        {
            versions.push(id);
        }
    }
    versions.extend(
        sqlx::query_scalar::<_, i64>(
            "SELECT g.access_plan_version_id FROM project_access_grants g \
             JOIN access_plan_versions v ON v.id=g.access_plan_version_id \
             JOIN access_plans p ON p.id=v.access_plan_id \
             WHERE g.project_id=$1 AND g.status='active' AND p.status='enabled' AND v.status='published' \
             AND (g.valid_from IS NULL OR g.valid_from<=$2) AND (g.valid_until IS NULL OR g.valid_until>$2) \
             AND (v.effective_start_at IS NULL OR v.effective_start_at<=$2) \
             AND (v.effective_end_at IS NULL OR v.effective_end_at>$2) ORDER BY g.id",
        )
        .bind(project_id)
        .bind(now)
        .fetch_all(pool)
        .await
        .map_err(query)?,
    );
    versions.sort_unstable();
    versions.dedup();

    let mut route_ids = BTreeMap::<i64, BTreeSet<i64>>::new();
    let mut upstream_ids = BTreeMap::<i64, BTreeMap<i64, String>>::new();
    for version_id in versions {
        let model_ids = sqlx::query_scalar::<_, i64>(
            "SELECT public_model_id FROM access_plan_items WHERE access_plan_version_id=$1",
        )
        .bind(version_id)
        .fetch_all(pool)
        .await
        .map_err(query)?;
        let selected = sqlx::query_scalar::<_, i64>(
            "SELECT model_route_id FROM access_plan_route_items WHERE access_plan_version_id=$1",
        )
        .bind(version_id)
        .fetch_all(pool)
        .await
        .map_err(query)?
        .into_iter()
        .collect::<BTreeSet<_>>();
        for model_id in model_ids {
            let rows = sqlx::query(
                "SELECT r.id AS route_id,d.channel_id,d.upstream_model_id FROM model_routes r \
                 JOIN upstream_model_deployments d ON d.id=r.deployment_id JOIN channels c ON c.id=d.channel_id \
                 WHERE r.public_model_id=$1 AND r.status='enabled' AND d.status='enabled' \
                 AND c.status='enabled' AND c.deleted_at=0 ORDER BY d.channel_id,r.id",
            )
            .bind(model_id)
            .fetch_all(pool)
            .await
            .map_err(query)?;
            for row in rows {
                let route_id: i64 = row.get("route_id");
                if !selected.is_empty() && !selected.contains(&route_id) {
                    continue;
                }
                let channel_id: i64 = row.get("channel_id");
                route_ids.entry(model_id).or_default().insert(channel_id);
                upstream_ids
                    .entry(model_id)
                    .or_default()
                    .entry(channel_id)
                    .or_insert_with(|| row.get("upstream_model_id"));
            }
        }
    }

    for row in sqlx::query(
        "SELECT public_model_id,effect FROM project_entitlement_overrides WHERE project_id=$1 \
         AND status='active' AND (valid_from IS NULL OR valid_from<=$2) \
         AND (valid_until IS NULL OR valid_until>$2) ORDER BY id",
    )
    .bind(project_id)
    .bind(now)
    .fetch_all(pool)
    .await
    .map_err(query)?
    {
        let model_id: i64 = row.get("public_model_id");
        if row.get::<String, _>("effect") == "block" {
            route_ids.remove(&model_id);
            upstream_ids.remove(&model_id);
            continue;
        }
        for route in sqlx::query(
            "SELECT d.channel_id,d.upstream_model_id FROM model_routes r \
             JOIN upstream_model_deployments d ON d.id=r.deployment_id JOIN channels c ON c.id=d.channel_id \
             WHERE r.public_model_id=$1 AND r.status='enabled' AND d.status='enabled' \
             AND c.status='enabled' AND c.deleted_at=0 ORDER BY d.channel_id,r.id",
        )
        .bind(model_id)
        .fetch_all(pool)
        .await
        .map_err(query)?
        {
            let channel_id: i64 = route.get("channel_id");
            route_ids.entry(model_id).or_default().insert(channel_id);
            upstream_ids.entry(model_id).or_default().entry(channel_id)
                .or_insert_with(|| route.get("upstream_model_id"));
        }
    }
    let models =
        sqlx::query("SELECT id,model_id FROM models WHERE status='enabled' AND deleted_at=0")
            .fetch_all(pool)
            .await
            .map_err(query)?
            .into_iter()
            .map(|r| (r.get::<i64, _>("id"), r.get::<String, _>("model_id")))
            .collect::<BTreeMap<_, _>>();
    Ok(EffectiveProjectAccess {
        routes_by_model: route_ids
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .filter_map(|(id, v)| models.get(&id).cloned().map(|m| (m, v)))
            .collect(),
        upstream_models_by_model: upstream_ids
            .into_iter()
            .filter_map(|(id, v)| models.get(&id).cloned().map(|m| (m, v)))
            .collect(),
        // Pricing is resolved by the accounting settler. Authorization must
        // not be denied merely because a project has no explicit price tier.
        price_multiplier: Decimal::ONE,
    })
}

pub async fn resolve_effective_project_price_multiplier_postgres(
    pool: &PgPool,
    project_id: i64,
    now: DateTime<Utc>,
) -> Result<Decimal, ModelCatalogError> {
    let tier_id = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT base_price_tier_id FROM project_commercial_profiles WHERE project_id=$1 AND status='active'",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(query)?
    .flatten();
    let base = if let Some(tier_id) = tier_id {
        sqlx::query_scalar::<_, i64>(
            "SELECT multiplier_ppm FROM price_tiers WHERE id=$1 AND status='enabled'",
        )
        .bind(tier_id)
        .fetch_optional(pool)
        .await
        .map_err(query)?
        .unwrap_or(MULTIPLIER_ONE_PPM)
    } else {
        MULTIPLIER_ONE_PPM
    };
    let rows = sqlx::query(
        "SELECT id,multiplier_ppm,stacking_key,priority,source_type,source_id,status,valid_from,valid_until \
         FROM project_price_adjustments WHERE project_id=$1",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(query)?;
    let adjustments = rows
        .into_iter()
        .map(|row| ProjectPriceAdjustment {
            id: row.get("id"),
            multiplier_ppm: row.get("multiplier_ppm"),
            stacking_key: row.get("stacking_key"),
            priority: row.get("priority"),
            source_type: row.get("source_type"),
            source_id: row.get("source_id"),
            status: row.get("status"),
            valid_from: row.get("valid_from"),
            valid_until: row.get("valid_until"),
        })
        .collect::<Vec<_>>();
    let resolved = resolve_project_price_multiplier(base, &adjustments, now)
        .map_err(|error| ModelCatalogError::Query(error.to_string()))?;
    Ok(Decimal::from(resolved.effective_multiplier_ppm) / Decimal::from(MULTIPLIER_ONE_PPM))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn postgres_resolver_keeps_public_model_channel_route_identity_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = std::process::id();
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,status,description,profiles) VALUES($1,'active','','{}'::jsonb) RETURNING id",
        ).bind(format!("pg-access-project-{suffix}")).fetch_one(&pool).await?;
        let model_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO models(developer,model_id,name,icon,\"group\",model_card,settings,status) \
             VALUES('test',$1,$1,'','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id",
        )
        .bind(format!("pg-public-model-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels(\"type\",name,status,credentials,default_test_model) \
             VALUES('openai',$1,'enabled','{}'::jsonb,'upstream-model') RETURNING id",
        )
        .bind(format!("pg-channel-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let deployment_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,status) \
             VALUES($1,'upstream-model','upstream-model','enabled') RETURNING id",
        ).bind(channel_id).fetch_one(&pool).await?;
        let route_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO model_routes(public_model_id,deployment_id,status) VALUES($1,$2,'enabled') RETURNING id",
        ).bind(model_id).bind(deployment_id).fetch_one(&pool).await?;
        let plan_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO access_plans(name,status,is_default,created_at,updated_at) VALUES($1,'enabled',FALSE,now(),now()) RETURNING id",
        ).bind(format!("pg-plan-{suffix}")).fetch_one(&pool).await?;
        let version_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO access_plan_versions(access_plan_id,version,status,reference_id,created_at,updated_at) \
             VALUES($1,1,'published',$2,now(),now()) RETURNING id",
        ).bind(plan_id).bind(format!("pg-plan-version-{suffix}")).fetch_one(&pool).await?;
        sqlx::query("INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES($1,$2,now())")
            .bind(version_id).bind(model_id).execute(&pool).await?;
        sqlx::query("INSERT INTO access_plan_route_items(access_plan_version_id,model_route_id,created_at) VALUES($1,$2,now())")
            .bind(version_id).bind(route_id).execute(&pool).await?;
        sqlx::query("INSERT INTO project_access_grants(project_id,access_plan_version_id,source_type,source_id,status,created_at,updated_at) \
                     VALUES($1,$2,'test',$3,'active',now(),now())")
            .bind(project_id).bind(version_id).bind(format!("pg-grant-{suffix}")).execute(&pool).await?;

        let access = resolve_effective_project_access_postgres(&pool, project_id).await?;
        let public_name = format!("pg-public-model-{suffix}");
        assert_eq!(
            access.channels_for_model(&public_name),
            BTreeSet::from([channel_id])
        );
        assert_eq!(
            access
                .upstream_models_for_model(&public_name)
                .get(&channel_id)
                .map(String::as_str),
            Some("upstream-model")
        );
        Ok(())
    }
}

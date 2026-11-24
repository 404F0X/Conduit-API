//! PostgreSQL channel model auto-sync worker adapter.

use chrono::{DateTime, Utc};
use conduit_core::objects::channel_settings::{
    AutoModelMappingRule, ChannelCredentials, ChannelSettings, DisabledAPIKey,
};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_scheduler::{AlignInterval, AutoSyncFrequency, align_to_interval};
use conduit_services::{
    SystemService, system_service::AutoSyncFrequency as StoredAutoSyncFrequency,
};
use regex::{Captures, Regex};
use sqlx::{FromRow, PgPool, Postgres, Transaction, types::Json};
use std::sync::{Arc, Mutex};

#[derive(Debug, FromRow)]
struct AutoSyncChannel {
    id: i64,
    name: String,
    channel_type: String,
    base_url: Option<String>,
    credentials: Json<serde_json::Value>,
    disabled_api_keys: Json<serde_json::Value>,
    manual_models: Option<Json<Vec<String>>>,
    auto_sync_model_pattern: Option<String>,
    settings: Json<serde_json::Value>,
}

struct CompiledAutoModelMappingRule {
    pattern: Regex,
    rule: AutoModelMappingRule,
}

pub(crate) struct PgChannelModelSyncAdapter {
    pool: PgPool,
    client: reqwest::Client,
    system: Option<Arc<SystemService>>,
    last_dynamic_run: Mutex<Option<DateTime<Utc>>>,
}

impl PgChannelModelSyncAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            pool,
            client: reqwest::Client::new(),
            system: None,
            last_dynamic_run: Mutex::new(None),
        }
    }

    pub(crate) fn with_dynamic_settings(mut self, system: Arc<SystemService>) -> Self {
        self.system = Some(system);
        self
    }

    pub(crate) async fn run(&self) -> Result<(), String> {
        if !crate::wiring_product_experience::feature_enabled(&self.pool, "automatic_model_sync")
            .await?
        {
            tracing::debug!("automatic model sync skipped: FEATURE_DISABLED");
            return Ok(());
        }
        if !self.should_run_for_current_settings(Utc::now()).await {
            return Ok(());
        }
        let channels = sqlx::query_as::<_, AutoSyncChannel>(
            "SELECT id,name,\"type\" AS channel_type,base_url,credentials,manual_models, \
                    COALESCE(disabled_api_keys,'[]'::jsonb) AS disabled_api_keys, \
                    auto_sync_model_pattern,settings \
             FROM channels WHERE status='enabled' AND deleted_at=0 \
               AND auto_sync_supported_models=TRUE ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        let mut failures = Vec::new();
        for channel in channels {
            if !crate::wiring_product_experience::feature_enabled(
                &self.pool,
                "automatic_model_sync",
            )
            .await?
            {
                break;
            }
            let settings = match &self.system {
                Some(system) => Some(
                    system
                        .channel_setting_or_default(&RequestContext::new(PolicyContext::new(
                            Principal::system(),
                        )))
                        .await,
                ),
                None => None,
            };
            let frequency = settings
                .as_ref()
                .map(|s| stored_auto_sync_frequency(&s.auto_sync.frequency.0))
                .unwrap_or(AutoSyncFrequency::OneHour);
            let bucket = align_to_interval(
                AlignInterval::from_auto_sync_frequency(frequency),
                Utc::now(),
            );
            let key = format!("model-sync:{}:{}", channel.id, bucket.timestamp());
            if let Err(error) = crate::maintenance_claim::run(
                &self.pool,
                &key,
                self.system.is_some(),
                self.sync_one(&channel),
            )
            .await
            {
                tracing::warn!(
                    channel_id = channel.id,
                    channel_name = %channel.name,
                    %error,
                    "PostgreSQL channel model sync failed"
                );
                failures.push(format!("{}: {error}", channel.name));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "{} channel model sync(s) failed: {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    async fn should_run_for_current_settings(&self, now: DateTime<Utc>) -> bool {
        let Some(system) = &self.system else {
            return true;
        };
        let context = RequestContext::new(PolicyContext::new(Principal::system()));
        let settings = system.channel_setting_or_default(&context).await;
        let frequency = stored_auto_sync_frequency(&settings.auto_sync.frequency.0);
        let aligned = align_to_interval(AlignInterval::from_auto_sync_frequency(frequency), now);
        record_new_dynamic_bucket(&self.last_dynamic_run, aligned)
    }

    async fn sync_one(&self, channel: &AutoSyncChannel) -> Result<(), String> {
        let api_keys = enabled_model_sync_api_keys(
            channel.credentials.0.clone(),
            channel.disabled_api_keys.0.clone(),
        )?;
        let mut failures = Vec::new();
        let mut fetched = None;
        for (index, api_key) in api_keys.iter().enumerate() {
            let result = crate::model_fetch::fetch_models(
                &self.client,
                &channel.channel_type,
                channel.base_url.as_deref().unwrap_or_default(),
                api_key,
            )
            .await;
            if let Some(error) = result.error.as_deref() {
                failures.push(format!("credential #{}: {error}", index + 1));
            } else {
                fetched = Some(result);
                break;
            }
        }
        let fetched = fetched.ok_or_else(|| {
            format!(
                "model fetch failed for all {} enabled credential(s): {}",
                api_keys.len(),
                failures.join("; ")
            )
        })?;
        persist_fetched_models(
            &self.pool,
            channel,
            fetched,
            &crate::wiring_postgres_change_sets::OperationContext::automatic(),
            None,
        )
        .await
        .map(|_| ())
    }
}

pub(crate) async fn commit_model_discovery(
    pool: &PgPool,
    channel_id: i64,
    fetched: crate::model_fetch::FetchedModels,
    context: &crate::wiring_postgres_change_sets::OperationContext,
    fence: Option<&crate::wiring_postgres_channel_discovery::DiscoveryFence>,
) -> Result<usize, String> {
    let channel=sqlx::query_as::<_,AutoSyncChannel>("SELECT id,name,\"type\" AS channel_type,base_url,credentials,COALESCE(disabled_api_keys,'[]'::jsonb) AS disabled_api_keys,manual_models,auto_sync_model_pattern,settings FROM channels WHERE id=$1 AND deleted_at=0").bind(channel_id).fetch_one(pool).await.map_err(|e|e.to_string())?;
    persist_fetched_models(pool, &channel, fetched, context, fence).await
}
async fn persist_fetched_models(
    pool: &PgPool,
    channel: &AutoSyncChannel,
    fetched: crate::model_fetch::FetchedModels,
    context: &crate::wiring_postgres_change_sets::OperationContext,
    fence: Option<&crate::wiring_postgres_channel_discovery::DiscoveryFence>,
) -> Result<usize, String> {
    let regex = channel
        .auto_sync_model_pattern
        .as_deref()
        .filter(|pattern| !pattern.is_empty())
        .map(regex::Regex::new)
        .transpose()
        .map_err(|error| format!("invalid auto-sync model pattern: {error}"))?;
    let settings: ChannelSettings = serde_json::from_value(channel.settings.0.clone())
        .map_err(|error| format!("invalid channel settings: {error}"))?;
    let mapping_rules = compile_auto_model_mapping_rules(settings.auto_model_mapping_rules)?;
    let mut merged = channel
        .manual_models
        .as_ref()
        .map(|models| models.0.clone())
        .unwrap_or_default();
    let mut discovered = Vec::new();
    for model in fetched.model_ids {
        if regex
            .as_ref()
            .is_some_and(|filter| !filter.is_match(&model))
        {
            continue;
        }
        if !merged.contains(&model) {
            merged.push(model.clone());
        }
        discovered.push(model);
    }
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    let policy = crate::wiring_governance::lock_policy(&mut tx).await?;
    if let Some(fence) = fence {
        crate::wiring_postgres_channel_discovery::lock_fence(&mut tx, fence).await?;
    }
    crate::wiring_postgres_provider_pricing::lock_provider_pricing_channel(&mut tx, channel.id)
        .await?;
    let discovered_count = discovered.len();
    let observation:i64=sqlx::query_scalar("INSERT INTO channel_model_observations(channel_id,supported_models) VALUES($1,$2) RETURNING id").bind(channel.id).bind(Json(&merged)).fetch_one(&mut *tx).await.map_err(|_|"model observation storage failed")?;
    let supply_package = crate::wiring_governance_commands::stage_supply(
        &mut tx,
        context,
        channel.id,
        merged.clone(),
        observation,
        &policy,
    )
    .await?;
    let mut proposals = Vec::new();
    for upstream_model_id in discovered {
        if let Some(proposal) =
            apply_auto_model_mapping_rules(&mut tx, channel, &upstream_model_id, &mapping_rules)
                .await?
        {
            proposals.push(proposal);
        }
    }
    let package = crate::wiring_postgres_change_sets::packages::stage_observed(
        &mut tx,
        context,
        "model_mapping",
        channel.id,
        &format!("Model mapping discovery: {}", channel.name),
        &Utc::now().timestamp_micros().to_string(),
        &proposals,
        Utc::now(),
    )
    .await?;
    if let Some(fence) = fence {
        crate::wiring_postgres_channel_discovery::finish_models(
            &mut tx,
            fence,
            discovered_count,
            package.as_ref(),
        )
        .await?;
    }
    if let (Some(fence), Some(supply)) = (fence, supply_package) {
        sqlx::query("UPDATE channel_operation_receipts SET stage_results=jsonb_set(stage_results,'{models,supplyChangeSetID}',to_jsonb($2::bigint)) WHERE operation_id=$1").bind(fence.operation_id).bind(supply).execute(&mut *tx).await.map_err(|_|"supply receipt failed")?;
    }
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(discovered_count)
}

fn enabled_model_sync_api_keys(
    credentials: serde_json::Value,
    disabled_api_keys: serde_json::Value,
) -> Result<Vec<String>, String> {
    let credentials: ChannelCredentials = serde_json::from_value(credentials)
        .map_err(|error| format!("invalid credentials: {error}"))?;
    let disabled: Vec<DisabledAPIKey> = serde_json::from_value(disabled_api_keys)
        .map_err(|error| format!("invalid disabled API keys: {error}"))?;
    credentials
        .get_enabled_api_keys(&disabled)
        .filter(|keys| !keys.is_empty())
        .ok_or_else(|| "an enabled API key is required".to_string())
}

fn stored_auto_sync_frequency(value: &str) -> AutoSyncFrequency {
    match value {
        StoredAutoSyncFrequency::SIX_HOURS => AutoSyncFrequency::SixHours,
        StoredAutoSyncFrequency::ONE_DAY => AutoSyncFrequency::OneDay,
        _ => AutoSyncFrequency::OneHour,
    }
}

fn record_new_dynamic_bucket(
    last_run: &Mutex<Option<DateTime<Utc>>>,
    aligned: DateTime<Utc>,
) -> bool {
    let Ok(mut last_run) = last_run.lock() else {
        return false;
    };
    if last_run.as_ref() == Some(&aligned) {
        return false;
    }
    *last_run = Some(aligned);
    true
}

fn compile_auto_model_mapping_rules(
    rules: Vec<AutoModelMappingRule>,
) -> Result<Vec<CompiledAutoModelMappingRule>, String> {
    const MODEL_TYPES: &[&str] = &[
        "chat",
        "embedding",
        "rerank",
        "image_generation",
        "video_generation",
    ];
    rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            if rule.public_model_id_template.trim().is_empty() {
                return Err(format!(
                    "auto model mapping rule {} has an empty public model template",
                    index + 1
                ));
            }
            if !MODEL_TYPES.contains(&rule.model_type.as_str()) {
                return Err(format!(
                    "auto model mapping rule {} has unsupported model type {:?}",
                    index + 1,
                    rule.model_type
                ));
            }
            let pattern = Regex::new(&rule.pattern).map_err(|error| {
                format!("invalid auto model mapping rule {}: {error}", index + 1)
            })?;
            Ok(CompiledAutoModelMappingRule { pattern, rule })
        })
        .collect()
}

async fn apply_auto_model_mapping_rules(
    tx: &mut Transaction<'_, Postgres>,
    channel: &AutoSyncChannel,
    upstream_model_id: &str,
    rules: &[CompiledAutoModelMappingRule],
) -> Result<Option<crate::wiring_postgres_change_sets::packages::Proposal>, String> {
    let Some((compiled, captures)) = first_matching_rule(rules, upstream_model_id) else {
        return Ok(None);
    };
    let public_model_key = expand_template(&captures, &compiled.rule.public_model_id_template);
    if public_model_key.trim().is_empty() {
        tracing::warn!(
            channel_id = channel.id,
            upstream_model_id,
            "auto model mapping produced an empty public model id"
        );
        return Ok(None);
    }
    let public_model_key = public_model_key.trim().to_string();

    let deployment_id=sqlx::query_scalar::<_,i64>("SELECT id FROM upstream_model_deployments WHERE channel_id=$1 AND upstream_model_id=$2 AND variant='' LIMIT 1").bind(channel.id).bind(upstream_model_id).fetch_optional(&mut **tx).await.map_err(|_|"deployment observation lookup failed")?;

    let existing = sqlx::query(
        "SELECT id,status,developer,\"type\",name,\"group\" FROM models \
         WHERE model_id=$1 AND deleted_at=0 LIMIT 1",
    )
    .bind(&public_model_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    if existing
        .as_ref()
        .is_some_and(|row| sqlx::Row::get::<String, _>(row, "status") == "archived")
    {
        tracing::warn!(
            channel_id = channel.id,
            upstream_model_id,
            public_model_id = %public_model_key,
            "auto model mapping skipped an archived public model"
        );
        return Ok(None);
    }
    if existing.is_none() && !compiled.rule.create_draft {
        return Ok(None);
    }
    let existing_route_status = match existing.as_ref() {
        Some(row) => sqlx::query_scalar::<_, String>(
            "SELECT status FROM model_routes WHERE public_model_id=$1 AND deployment_id=$2",
        )
        .bind(sqlx::Row::get::<i64, _>(row, "id"))
        .bind(deployment_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| error.to_string())?,
        None => None,
    };
    if existing_route_status.as_deref() == Some("enabled") {
        return Ok(None);
    }
    let developer = expanded_or(
        &captures,
        &compiled.rule.developer_template,
        &channel.channel_type,
    );
    let name = expanded_or(&captures, &compiled.rule.name_template, &public_model_key);
    let group = expanded_or(&captures, &compiled.rule.group_template, &developer);
    let before_snapshot = existing.as_ref().map(|row| {
        serde_json::json!({
            "id": sqlx::Row::get::<i64, _>(row, "id"),
            "status": sqlx::Row::get::<String, _>(row, "status"),
            "developer": sqlx::Row::get::<String, _>(row, "developer"),
            "type": sqlx::Row::get::<String, _>(row, "type"),
            "name": sqlx::Row::get::<String, _>(row, "name"),
            "group": sqlx::Row::get::<String, _>(row, "group"),
            "routeStatus": existing_route_status,
        })
    });
    let after_snapshot = serde_json::json!({
        "deploymentID": deployment_id,
        "upstreamModelID": upstream_model_id,
        "publicModel": {
            "modelID": public_model_key,
            "developer": developer,
            "type": compiled.rule.model_type,
            "name": name,
            "group": group,
        }
    });
    Ok(Some(
        crate::wiring_postgres_change_sets::packages::Proposal {
            key: upstream_model_id.to_string(),
            action: if before_snapshot.is_some() {
                "update"
            } else {
                "create"
            }
            .into(),
            before: before_snapshot,
            after: Some(after_snapshot),
            source: serde_json::json!({"source":"observed","channelID":channel.id,"channelName":channel.name,"upstreamModelID":upstream_model_id}),
            error: None,
        },
    ))
}

fn first_matching_rule<'r, 'm>(
    rules: &'r [CompiledAutoModelMappingRule],
    upstream_model_id: &'m str,
) -> Option<(&'r CompiledAutoModelMappingRule, Captures<'m>)> {
    rules.iter().find_map(|rule| {
        rule.pattern
            .captures(upstream_model_id)
            .map(|captures| (rule, captures))
    })
}

fn expand_template(captures: &Captures<'_>, template: &str) -> String {
    let mut expanded = String::new();
    captures.expand(template, &mut expanded);
    expanded
}

fn expanded_or(captures: &Captures<'_>, template: &str, fallback: &str) -> String {
    let expanded = expand_template(captures, template);
    let expanded = expanded.trim();
    if expanded.is_empty() {
        fallback.to_string()
    } else {
        expanded.to_string()
    }
}

impl conduit_scheduler::ChannelModelSyncExecutor for PgChannelModelSyncAdapter {
    fn sync_models(&self) -> Result<(), String> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(self.run()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::change_set::ChangeSetServices as _;
    use conduit_scheduler::ChannelModelSyncExecutor;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn dynamic_auto_sync_frequency_uses_the_current_time_bucket()
    -> Result<(), Box<dyn std::error::Error>> {
        let last_run = Mutex::new(None);
        let first = DateTime::parse_from_rfc3339("2024-01-01T07:15:00Z")?.with_timezone(&Utc);
        let same_six_hour_bucket =
            DateTime::parse_from_rfc3339("2024-01-01T11:59:00Z")?.with_timezone(&Utc);

        let six_hours = AlignInterval::from_auto_sync_frequency(stored_auto_sync_frequency("6h"));
        assert!(record_new_dynamic_bucket(
            &last_run,
            align_to_interval(six_hours, first)
        ));
        assert!(!record_new_dynamic_bucket(
            &last_run,
            align_to_interval(six_hours, same_six_hour_bucket)
        ));

        let one_hour = AlignInterval::from_auto_sync_frequency(stored_auto_sync_frequency("1h"));
        assert!(record_new_dynamic_bucket(
            &last_run,
            align_to_interval(one_hour, same_six_hour_bucket)
        ));
        assert_eq!(
            stored_auto_sync_frequency("invalid"),
            AutoSyncFrequency::OneHour
        );
        Ok(())
    }

    #[test]
    fn model_sync_preserves_all_enabled_api_keys_in_credential_order() -> Result<(), String> {
        let selected = enabled_model_sync_api_keys(
            json!({"apiKeys": ["disabled-key", "enabled-key"]}),
            json!([{"key": "disabled-key"}]),
        )?;
        assert_eq!(selected, vec!["enabled-key"]);
        Ok(())
    }

    async fn publish_reviewed_mapping(
        pool: &PgPool,
        id: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use conduit_admin_graphql::change_package::{
            ChangePackageServices, ChangeSetAvailableAction, PackageViewer,
        };
        let adapter = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
        let detail = adapter
            .get_detail(
                PackageViewer {
                    actor_user_id: 1,
                    can_write: true,
                },
                async_graphql::ID(id.to_string()),
            )
            .await?
            .ok_or("missing observed package")?;
        assert_eq!(
            detail.summary.status,
            conduit_admin_graphql::change_set::ChangeSetStatus::Approved
        );
        adapter
            .transition(
                1,
                detail.summary.id,
                detail.summary.revision,
                ChangeSetAvailableAction::Publish,
                None,
            )
            .await?;
        Ok(())
    }
    async fn publish_observed_supply(
        pool: &PgPool,
        channel: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use conduit_admin_graphql::change_package::{
            ChangePackageServices, ChangeSetAvailableAction, PackageViewer,
        };
        let id:i64=sqlx::query_scalar("SELECT id FROM change_sets WHERE kind='model_supply' AND scope_id=$1 AND status='pending_review' ORDER BY id DESC LIMIT 1").bind(channel.to_string()).fetch_one(pool).await?;
        let adapter = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
        let detail = adapter
            .get_detail(
                PackageViewer {
                    actor_user_id: 1,
                    can_write: true,
                },
                async_graphql::ID(id.to_string()),
            )
            .await?
            .ok_or("missing supply package")?;
        adapter
            .transition(
                1,
                detail.summary.id,
                detail.summary.revision,
                ChangeSetAvailableAction::ApproveApply,
                None,
            )
            .await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_model_sync_tracks_provider_removal_and_recovery_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer failing-sync-key"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sync-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id":"gpt-expensive"},{"id":"embed-small"},{"id":"gpt-cheap"}]
            })))
            .mount(&server)
            .await;
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        database.owners(&[1]).await?;
        let pool = database.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let public_model_prefix = format!("sync-draft-{suffix}");
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",base_url,name,status,credentials,disabled_api_keys,supported_models,manual_models, \
               auto_sync_supported_models,auto_sync_model_pattern,default_test_model,settings) \
             VALUES('openai',$1,$2,'enabled',$3,$4,'[]'::jsonb,$5,TRUE,'^gpt-','gpt-cheap',$6) \
             RETURNING id",
        )
        .bind(format!("{}/v1", server.uri()))
        .bind(format!("PG auto-sync {suffix}"))
        .bind(Json(json!({"apiKeys":["disabled-sync-key", "failing-sync-key", "sync-key"]})))
        .bind(Json(json!([{"key":"disabled-sync-key"}])))
        .bind(Json(json!(["manual-model"])))
        .bind(Json(json!({
            "autoModelMappingRules": [{
                "pattern": "^gpt-(?<tier>.+)$",
                "publicModelIdTemplate": format!("{public_model_prefix}-$tier"),
                "createDraft": true,
                "developerTemplate": "openai",
                "modelType": "chat"
            }]
        })))
        .fetch_one(&pool)
        .await?;

        let adapter = PgChannelModelSyncAdapter::new(pool.clone());
        tokio::task::spawn_blocking(move || adapter.sync_models()).await??;
        assert_eq!(
            sqlx::query_scalar::<_, Json<Vec<String>>>(
                "SELECT supported_models FROM channels WHERE id=$1"
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await?
            .0,
            Vec::<String>::new(),
            "discovery must leave online supply unchanged before publication"
        );
        publish_observed_supply(&pool, channel_id).await?;
        let models = sqlx::query_scalar::<_, Json<Vec<String>>>(
            "SELECT supported_models FROM channels WHERE id=$1",
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await?
        .0;
        assert_eq!(models, vec!["manual-model", "gpt-expensive", "gpt-cheap"]);
        let formal_mapping_count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM model_routes route JOIN upstream_model_deployments deployment \
             ON deployment.id=route.deployment_id WHERE deployment.channel_id=$1",
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await?;
        assert_eq!(formal_mapping_count, 0);
        let pending_mappings = sqlx::query_as::<_, (i64, String, String)>(
            "SELECT cs.id,item.after_snapshot->'publicModel'->>'modelID',cs.status \
             FROM change_sets cs JOIN change_set_items item ON item.change_set_id=cs.id \
             WHERE cs.kind='model_mapping' AND cs.scope_id=$1 ORDER BY item.item_key",
        )
        .bind(channel_id.to_string())
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            pending_mappings
                .iter()
                .map(|(_, model, status)| (model.clone(), status.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    format!("{public_model_prefix}-cheap"),
                    "pending_review".into()
                ),
                (
                    format!("{public_model_prefix}-expensive"),
                    "pending_review".into()
                )
            ]
        );
        let change_sets = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
        let package_ids = pending_mappings
            .into_iter()
            .map(|(id, _, _)| id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            package_ids.len(),
            1,
            "one synchronization produces one complete mapping package"
        );
        for id in package_ids {
            change_sets
                .approve_change_set(1, id.to_string().into(), None)
                .await?;
            publish_reviewed_mapping(&pool, id).await?;
        }
        let applied_mappings = sqlx::query_as::<_, (String, String, String)>(
            "SELECT model.model_id,model.status,route.status FROM models model \
             JOIN model_routes route ON route.public_model_id=model.id \
             JOIN upstream_model_deployments deployment ON deployment.id=route.deployment_id \
             WHERE deployment.channel_id=$1 ORDER BY model.model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            applied_mappings,
            vec![
                (
                    format!("{public_model_prefix}-cheap"),
                    "enabled".into(),
                    "enabled".into()
                ),
                (
                    format!("{public_model_prefix}-expensive"),
                    "enabled".into(),
                    "enabled".into()
                )
            ]
        );
        let initial_deployments = sqlx::query_as::<_, (String, String)>(
            "SELECT upstream_model_id,status FROM upstream_model_deployments \
             WHERE channel_id=$1 ORDER BY upstream_model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            initial_deployments,
            vec![
                ("gpt-cheap".into(), "enabled".into()),
                ("gpt-expensive".into(), "enabled".into())
            ]
        );

        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sync-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "temporarily unavailable"
            })))
            .mount(&server)
            .await;
        let adapter = PgChannelModelSyncAdapter::new(pool.clone());
        let malformed_result = tokio::task::spawn_blocking(move || adapter.sync_models()).await?;
        assert!(malformed_result.is_err());
        let deployments_after_malformed = sqlx::query_as::<_, (String, String)>(
            "SELECT upstream_model_id,status FROM upstream_model_deployments \
             WHERE channel_id=$1 ORDER BY upstream_model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(deployments_after_malformed, initial_deployments);

        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sync-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": [] })))
            .mount(&server)
            .await;
        let adapter = PgChannelModelSyncAdapter::new(pool.clone());
        tokio::task::spawn_blocking(move || adapter.sync_models()).await??;
        assert_eq!(
            sqlx::query_scalar::<_, Json<Vec<String>>>(
                "SELECT supported_models FROM channels WHERE id=$1"
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await?
            .0,
            models,
            "removal observations must await publication"
        );
        publish_observed_supply(&pool, channel_id).await?;
        let models_after_removal = sqlx::query_scalar::<_, Json<Vec<String>>>(
            "SELECT supported_models FROM channels WHERE id=$1",
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await?
        .0;
        assert_eq!(models_after_removal, vec!["manual-model"]);
        let removed_deployments = sqlx::query_as::<_, (String, String)>(
            "SELECT upstream_model_id,status FROM upstream_model_deployments \
             WHERE channel_id=$1 ORDER BY upstream_model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            removed_deployments,
            vec![
                ("gpt-cheap".into(), "disabled".into()),
                ("gpt-expensive".into(), "disabled".into())
            ]
        );
        let removed_routes = sqlx::query_as::<_, (String, String)>(
            "SELECT model.model_id,route.status FROM model_routes route \
             JOIN models model ON model.id=route.public_model_id \
             JOIN upstream_model_deployments deployment ON deployment.id=route.deployment_id \
             WHERE deployment.channel_id=$1 ORDER BY model.model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert!(
            removed_routes
                .iter()
                .all(|(_, status)| status == "disabled")
        );

        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer sync-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"id":"gpt-cheap"}]
            })))
            .mount(&server)
            .await;
        let adapter = PgChannelModelSyncAdapter::new(pool.clone());
        tokio::task::spawn_blocking(move || adapter.sync_models()).await??;
        publish_observed_supply(&pool, channel_id).await?;
        let recovered_deployments = sqlx::query_as::<_, (String, String)>(
            "SELECT upstream_model_id,status FROM upstream_model_deployments \
             WHERE channel_id=$1 ORDER BY upstream_model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            recovered_deployments,
            vec![
                ("gpt-cheap".into(), "disabled".into()),
                ("gpt-expensive".into(), "disabled".into())
            ],
            "an observed recovery does not enable a deployment before its mapping is published"
        );
        let recovered_change_set_id = sqlx::query_scalar::<_, i64>(
            "SELECT cs.id FROM change_sets cs JOIN change_set_items item ON item.change_set_id=cs.id \
             WHERE cs.kind='model_mapping' AND cs.scope_id=$1 AND cs.status='pending_review' \
               AND item.item_key='gpt-cheap' ORDER BY cs.id DESC LIMIT 1",
        )
        .bind(channel_id.to_string())
        .fetch_one(&pool)
        .await?;
        let change_sets = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
        change_sets
            .approve_change_set(1, recovered_change_set_id.to_string().into(), None)
            .await?;
        publish_reviewed_mapping(&pool, recovered_change_set_id).await?;
        let recovered_routes = sqlx::query_as::<_, (String, String)>(
            "SELECT deployment.upstream_model_id,route.status FROM model_routes route \
             JOIN upstream_model_deployments deployment ON deployment.id=route.deployment_id \
             WHERE deployment.channel_id=$1 ORDER BY deployment.upstream_model_id",
        )
        .bind(channel_id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            recovered_routes,
            vec![
                ("gpt-cheap".into(), "enabled".into()),
                ("gpt-expensive".into(), "disabled".into())
            ]
        );

        sqlx::query(
            "DELETE FROM model_routes WHERE deployment_id IN \
             (SELECT id FROM upstream_model_deployments WHERE channel_id=$1)",
        )
        .bind(channel_id)
        .execute(&pool)
        .await?;
        sqlx::query("DELETE FROM models WHERE model_id LIKE $1")
            .bind(format!("{public_model_prefix}-%"))
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM upstream_model_deployments WHERE channel_id=$1")
            .bind(channel_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM channel_model_observations WHERE channel_id=$1")
            .bind(channel_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM channels WHERE id=$1")
            .bind(channel_id)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }

    #[test]
    fn mapping_templates_expand_named_captures_and_keep_first_match() -> Result<(), String> {
        let rules = compile_auto_model_mapping_rules(vec![
            AutoModelMappingRule {
                pattern: "^vendor/(?<family>[^:]+):(?<version>.+)$".into(),
                public_model_id_template: "$family-$version".into(),
                developer_template: "vendor".into(),
                ..Default::default()
            },
            AutoModelMappingRule {
                pattern: "^vendor/.+$".into(),
                public_model_id_template: "must-not-win".into(),
                ..Default::default()
            },
        ])?;
        let (rule, captures) = first_matching_rule(&rules, "vendor/reasoner:v3")
            .ok_or_else(|| "pattern did not match".to_string())?;
        assert_eq!(
            expand_template(&captures, &rule.rule.public_model_id_template),
            "reasoner-v3"
        );
        Ok(())
    }
}

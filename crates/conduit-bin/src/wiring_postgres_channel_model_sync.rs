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

    async fn run(&self) -> Result<(), String> {
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
            if let Err(error) = self.sync_one(&channel).await {
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
        let mut tx = self.pool.begin().await.map_err(|error| error.to_string())?;
        sqlx::query("UPDATE channels SET supported_models=$1,updated_at=now() WHERE id=$2")
            .bind(Json(&merged))
            .bind(channel.id)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
        sqlx::query(
            "UPDATE upstream_model_deployments SET status='disabled',updated_at=now() \
             WHERE channel_id=$1 AND source='discovered' AND status='enabled' \
               AND NOT (upstream_model_id=ANY($2))",
        )
        .bind(channel.id)
        .bind(&merged)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "UPDATE model_routes route SET status='disabled',updated_at=now() \
             FROM upstream_model_deployments deployment \
             WHERE route.deployment_id=deployment.id AND deployment.channel_id=$1 \
               AND deployment.status='disabled' AND route.status='enabled'",
        )
        .bind(channel.id)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "WITH superseded AS (\
               UPDATE change_sets cs SET status='superseded',reviewed_at=now(),\
                      review_note='upstream deployment is no longer available',updated_at=now() \
               WHERE cs.kind='model_mapping' AND cs.scope_type='channel' AND cs.scope_id=$1 \
                 AND cs.status='pending_review' AND EXISTS(\
                   SELECT 1 FROM change_set_items item \
                   JOIN upstream_model_deployments deployment \
                     ON deployment.id=CAST(item.after_snapshot->>'deploymentID' AS BIGINT) \
                   WHERE item.change_set_id=cs.id AND deployment.status<>'enabled') \
               RETURNING cs.id) \
             INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at) \
             SELECT id,'superseded','system','{\"reason\":\"upstream_removed\"}'::jsonb,now() FROM superseded",
        )
        .bind(channel.id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        for upstream_model_id in discovered {
            apply_auto_model_mapping_rules(&mut tx, channel, &upstream_model_id, &mapping_rules)
                .await?;
        }
        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(())
    }
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
) -> Result<(), String> {
    let Some((compiled, captures)) = first_matching_rule(rules, upstream_model_id) else {
        return Ok(());
    };
    let public_model_key = expand_template(&captures, &compiled.rule.public_model_id_template);
    if public_model_key.trim().is_empty() {
        tracing::warn!(
            channel_id = channel.id,
            upstream_model_id,
            "auto model mapping produced an empty public model id"
        );
        return Ok(());
    }
    let public_model_key = public_model_key.trim().to_string();

    let deployment_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO upstream_model_deployments \
         (channel_id,upstream_model_id,internal_name,variant,status,source,created_at,updated_at) \
         VALUES($1,$2,$3,'','enabled','discovered',now(),now()) \
         ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET \
           status='enabled',internal_name=EXCLUDED.internal_name,updated_at=now() RETURNING id",
    )
    .bind(channel.id)
    .bind(upstream_model_id)
    .bind(format!("{} / {upstream_model_id}", channel.name))
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;

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
        return Ok(());
    }
    if existing.is_none() && !compiled.rule.create_draft {
        return Ok(());
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
        supersede_model_mapping_change_sets(
            tx,
            &channel.id.to_string(),
            upstream_model_id,
            None,
            chrono::Utc::now(),
        )
        .await?;
        return Ok(());
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
    stage_model_mapping_change_set(
        tx,
        channel,
        upstream_model_id,
        before_snapshot,
        after_snapshot,
    )
    .await
}

async fn stage_model_mapping_change_set(
    tx: &mut Transaction<'_, Postgres>,
    channel: &AutoSyncChannel,
    upstream_model_id: &str,
    before_snapshot: Option<serde_json::Value>,
    after_snapshot: serde_json::Value,
) -> Result<(), String> {
    let scope_id = channel.id.to_string();
    let duplicate = sqlx::query_scalar::<_, i64>(
        "SELECT cs.id FROM change_sets cs JOIN change_set_items item ON item.change_set_id=cs.id \
         WHERE cs.kind='model_mapping' AND cs.scope_type='channel' AND cs.scope_id=$1 \
           AND cs.status='pending_review' AND item.item_key=$2 AND item.after_snapshot=$3 \
           AND item.before_snapshot IS NOT DISTINCT FROM $4 \
         ORDER BY cs.id DESC LIMIT 1",
    )
    .bind(&scope_id)
    .bind(upstream_model_id)
    .bind(Json(&after_snapshot))
    .bind(before_snapshot.as_ref().map(Json))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    let now = chrono::Utc::now();
    if let Some(id) = duplicate {
        supersede_model_mapping_change_sets(tx, &scope_id, upstream_model_id, Some(id), now)
            .await?;
        sqlx::query("UPDATE change_sets SET source_revision=$2,updated_at=$3 WHERE id=$1")
            .bind(id)
            .bind(now.timestamp_micros().to_string())
            .bind(now)
            .execute(&mut **tx)
            .await
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    supersede_model_mapping_change_sets(tx, &scope_id, upstream_model_id, None, now).await?;
    let change_set_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO change_sets \
         (kind,scope_type,scope_id,title,status,source_revision,submitted_at,created_at,updated_at) \
         VALUES('model_mapping','channel',$1,$2,'pending_review',$3,$4,$4,$4) RETURNING id",
    )
    .bind(&scope_id)
    .bind(format!("Model mapping: {upstream_model_id}"))
    .bind(now.timestamp_micros().to_string())
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(
        "INSERT INTO change_set_items \
         (change_set_id,item_key,action,before_snapshot,after_snapshot,source_snapshot,created_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$7)",
    )
    .bind(change_set_id)
    .bind(upstream_model_id)
    .bind(if before_snapshot.is_some() { "update" } else { "create" })
    .bind(before_snapshot.map(Json))
    .bind(Json(after_snapshot))
    .bind(Json(serde_json::json!({
        "channelID": channel.id,
        "channelName": channel.name,
        "upstreamModelID": upstream_model_id,
    })))
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    sqlx::query(
        "INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at) \
         VALUES($1,'submitted','system','{}'::jsonb,$2)",
    )
    .bind(change_set_id)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn supersede_model_mapping_change_sets(
    tx: &mut Transaction<'_, Postgres>,
    scope_id: &str,
    upstream_model_id: &str,
    except_id: Option<i64>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    sqlx::query(
        "WITH superseded AS (\
           UPDATE change_sets cs SET status='superseded',reviewed_at=$4,\
                  review_note='superseded by newer model discovery',updated_at=$4 \
           WHERE cs.kind='model_mapping' AND cs.scope_type='channel' AND cs.scope_id=$1 \
             AND cs.status='pending_review' AND ($3::BIGINT IS NULL OR cs.id<>$3) \
             AND EXISTS(SELECT 1 FROM change_set_items item WHERE item.change_set_id=cs.id AND item.item_key=$2) \
           RETURNING cs.id) \
         INSERT INTO change_set_events(change_set_id,event_type,actor_type,detail,created_at) \
         SELECT id,'superseded','system','{}'::jsonb,$4 FROM superseded",
    )
    .bind(scope_id)
    .bind(upstream_model_id)
    .bind(except_id)
    .bind(now)
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
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
        for (id, _, _) in pending_mappings {
            change_sets
                .approve_change_set(1, id.to_string().into(), None)
                .await?;
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
                ("gpt-cheap".into(), "enabled".into()),
                ("gpt-expensive".into(), "disabled".into())
            ]
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

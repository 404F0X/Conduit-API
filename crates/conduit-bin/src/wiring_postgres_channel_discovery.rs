//! Explicit NEWAPI initialization. Network work never holds a database lock.
use crate::wiring_postgres_change_sets::{OperationContext, packages::StagedPackage};
use async_graphql::ID;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_admin_graphql::{
    change_package::OperationOrigin,
    change_set::{ChangeSetKind, ChangeSetStatus},
    channel_discovery::*,
    scalars::TimeScalar,
};
use conduit_services::SystemService;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction, types::Json};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DiscoveryFence {
    pub operation_id: Uuid,
    pub attempt: i32,
}
#[derive(FromRow)]
struct Receipt {
    operation_id: Uuid,
    channel_id: i64,
    origin: String,
    created_by: Option<i64>,
    attempt: i32,
    state: String,
    stage_results: Json<Value>,
    lease_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
const RECEIPT_COLUMNS: &str = "operation_id,channel_id,origin,created_by,attempt,state,stage_results,lease_expires_at,created_at,updated_at";
pub(crate) struct PgChannelDiscoveryAdapter {
    pool: PgPool,
    system: Arc<SystemService>,
    client: reqwest::Client,
    recurring_price_enabled: bool,
}
impl PgChannelDiscoveryAdapter {
    pub(crate) fn new(
        pool: PgPool,
        system: Arc<SystemService>,
        recurring_price_enabled: bool,
    ) -> Self {
        Self {
            pool,
            system,
            client: reqwest::Client::new(),
            recurring_price_enabled,
        }
    }
    async fn receipt(&self, id: Uuid) -> Result<Option<Receipt>, String> {
        sqlx::query_as::<_,Receipt>(sqlx::AssertSqlSafe::<&str>(&format!("SELECT {RECEIPT_COLUMNS} FROM channel_operation_receipts WHERE operation_id=$1 AND operation_type='NEW_API_DISCOVERY'"))).bind(id).fetch_optional(&self.pool).await.map_err(db_error)
    }
    async fn dto(&self, row: Receipt) -> Result<ChannelDiscoveryOperation, String> {
        let expired =
            row.state == "RUNNING" && row.lease_expires_at.is_some_and(|t| t <= Utc::now());
        let models = stage_dto(&row.stage_results.0["models"], expired);
        let prices = stage_dto(&row.stage_results.0["prices"], expired);
        let mut package_ids = row.stage_results.0["packages"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(id) = row.stage_results.0["models"]["supplyChangeSetID"].as_i64() {
            if !package_ids.iter().any(|v| v["id"] == id) {
                package_ids.push(json!({"id":id,"reused":false}));
            }
        }
        let mut packages = Vec::new();
        let mut total = 0_i64;
        let mut invalid = 0_i64;
        let mut warnings = 0_i64;
        for item in package_ids {
            let Some(id) = item["id"].as_i64() else {
                continue;
            };
            let Some((kind,status,count,bad,warn))=sqlx::query_as::<_,(String,String,i64,i64,i64)>("SELECT c.kind,c.status,COUNT(i.id),COUNT(i.id) FILTER(WHERE i.validation_error IS NOT NULL),COUNT(i.id) FILTER(WHERE i.source_snapshot->'provenance'->'issues' @> '[{\"severity\":\"WARNING\"}]'::jsonb) FROM change_sets c LEFT JOIN change_set_items i ON i.change_set_id=c.id WHERE c.id=$1 GROUP BY c.id").bind(id).fetch_optional(&self.pool).await.map_err(db_error)?else{continue};
            total += count;
            invalid += bad;
            warnings += warn;
            packages.push(ChannelDiscoveryPackage {
                change_set_id: ID(format!("gid://conduit/ChangeSet/{id}")),
                kind: kind_from(&kind)?,
                status: status_from(&status)?,
                item_count: integer(count)?,
                reused: item["reused"].as_bool().unwrap_or(false),
            });
        }
        let (recurring,verified)=sqlx::query_as::<_,(bool,bool)>("SELECT c.auto_sync_supported_models,EXISTS(SELECT 1 FROM provider_quota_status q WHERE q.channel_id=c.id AND q.deleted_at=0 AND q.probe_adapter='new_api' AND q.probe_verified_at IS NOT NULL) FROM channels c WHERE c.id=$1 AND c.deleted_at=0").bind(row.channel_id).fetch_optional(&self.pool).await.map_err(db_error)?.unwrap_or((false,false));
        let enabled = self.recurring_price_enabled;
        let state = if expired {
            ChannelDiscoveryState::Interrupted
        } else {
            match row.state.as_str() {
                "NOT_STARTED" => ChannelDiscoveryState::NotStarted,
                "RUNNING" => ChannelDiscoveryState::Running,
                "PARTIAL" => ChannelDiscoveryState::Partial,
                "NEEDS_ACTION" => ChannelDiscoveryState::NeedsAction,
                "SUCCEEDED" => ChannelDiscoveryState::Succeeded,
                "INTERRUPTED" => ChannelDiscoveryState::Interrupted,
                _ => ChannelDiscoveryState::Failed,
            }
        };
        Ok(ChannelDiscoveryOperation {
            operation_id: ID(row.operation_id.to_string()),
            channel_id: ID(format!("gid://conduit/Channel/{}", row.channel_id)),
            origin: OperationOrigin::from_stored(&row.origin),
            created_by: row
                .created_by
                .map(|id| ID(format!("gid://conduit/User/{id}"))),
            created_at: TimeScalar(row.created_at),
            updated_at: TimeScalar(row.updated_at),
            attempt: row.attempt,
            state,
            channel_created: true,
            models,
            prices: prices.clone(),
            package_validation: ChannelDiscoveryPackageValidation {
                status: if invalid > 0 {
                    PackageValidationStatus::NeedsAction
                } else if total > 0 {
                    PackageValidationStatus::Valid
                } else if prices.status == ChannelDiscoveryStageStatus::NoChange {
                    PackageValidationStatus::NoChange
                } else {
                    PackageValidationStatus::Unchecked
                },
                total_items: integer(total)?,
                valid_items: integer(total - invalid)?,
                invalid_items: integer(invalid)?,
                warning_items: integer(warnings)?,
            },
            packages,
            lease_expires_at: row.lease_expires_at.map(TimeScalar),
            recurring_model_sync_enabled: recurring,
            recurring_price_discovery_state: if !enabled {
                RecurringPriceDiscoveryState::Disabled
            } else if verified {
                RecurringPriceDiscoveryState::Enabled
            } else {
                RecurringPriceDiscoveryState::AwaitingQuotaVerification
            },
            superseded_source_change_set_ids: row.stage_results.0["supersededSourceIDs"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(Value::as_i64)
                        .map(|i| ID(format!("gid://conduit/ChangeSet/{i}")))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
    async fn run_claimed(
        &self,
        actor: i64,
        fence: DiscoveryFence,
        stages: Vec<ChannelDiscoveryStage>,
    ) -> Result<ChannelDiscoveryOperation, String> {
        let row = self
            .receipt(fence.operation_id)
            .await?
            .ok_or("OPERATION_CONFLICT: operation missing")?;
        if row.created_by != Some(actor) {
            return Err("FORBIDDEN: only the discovery creator may execute this operation".into());
        }
        let channel_id = row.channel_id;
        let context = OperationContext {
            operation_id: fence.operation_id,
            origin: OperationOrigin::from_stored(&row.origin),
            actor_user_id: Some(actor),
        };
        let rebuild = row
            .stage_results
            .0
            .pointer("/request/rebuild")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value::<RebuildObservedPricePackagesInput>(v.clone()))
            .transpose()
            .map_err(|_| "OPERATION_CONFLICT: invalid saved rebuild request")?;
        let execute = async {
            for stage in stages {
                let result = match stage {
                    ChannelDiscoveryStage::Models => self
                        .fetch_models(channel_id)
                        .await
                        .and_then(|v| {
                            if v.error.is_some() {
                                Err("MODELS_FETCH_FAILED".into())
                            } else {
                                Ok(v)
                            }
                        })
                        .map(|v| v),
                    ChannelDiscoveryStage::Prices => {
                        let pricing =
                            crate::wiring_postgres_provider_quota::PgProviderQuotaAdapter::new(
                                self.pool.clone(),
                                self.system.clone(),
                            )
                            .fetch_pricing(channel_id)
                            .await;
                        match pricing {Ok(snapshot)=>{
                            match crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot_with_context(&self.pool,channel_id,"/api/pricing",&snapshot,&context,Some(&fence),rebuild.as_ref()).await {Ok(_)=>continue,Err(e)=>{self.fail_stage(&fence,stage,&e).await?;continue}}
                        },Err(e)=>{self.fail_stage(&fence,stage,&e).await?;continue}}
                    }
                };
                match result {
                    Ok(models) => {
                        if let Err(e) =
                            crate::wiring_postgres_channel_model_sync::commit_model_discovery(
                                &self.pool,
                                channel_id,
                                models,
                                &context,
                                Some(&fence),
                            )
                            .await
                        {
                            self.fail_stage(&fence, stage, &e).await?;
                        }
                    }
                    Err(e) => self.fail_stage(&fence, stage, &e).await?,
                }
            }
            Ok::<(), String>(())
        };
        match tokio::time::timeout(std::time::Duration::from_secs(90), execute).await {
            Ok(result) => result?,
            Err(_) => self.interrupt(&fence).await?,
        }
        self.get(ID(fence.operation_id.to_string()))
            .await?
            .ok_or_else(|| "OPERATION_CONFLICT: operation missing".into())
    }
    async fn fetch_models(&self, id: i64) -> Result<crate::model_fetch::FetchedModels, String> {
        let (kind,base,credentials,disabled)=sqlx::query_as::<_,(String,Option<String>,Json<Value>,Json<Value>)>("SELECT \"type\",base_url,credentials,COALESCE(disabled_api_keys,'[]'::jsonb) FROM channels WHERE id=$1 AND deleted_at=0").bind(id).fetch_one(&self.pool).await.map_err(db_error)?;
        let c: conduit_core::objects::channel_settings::ChannelCredentials =
            serde_json::from_value(credentials.0).map_err(|_| "CREDENTIALS_INVALID")?;
        let disabled: Vec<conduit_core::objects::channel_settings::DisabledAPIKey> =
            serde_json::from_value(disabled.0).map_err(|_| "CREDENTIALS_INVALID")?;
        let keys = c.get_enabled_api_keys(&disabled).ok_or("KEY_REQUIRED")?;
        for key in keys {
            let fetched = crate::model_fetch::fetch_models(
                &self.client,
                &kind,
                base.as_deref().unwrap_or_default(),
                &key,
            )
            .await;
            if fetched.error.is_none() {
                return Ok(fetched);
            }
        }
        Err("MODELS_FETCH_FAILED".into())
    }
    async fn fail_stage(
        &self,
        fence: &DiscoveryFence,
        stage: ChannelDiscoveryStage,
        error: &str,
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        lock_fence(&mut tx, fence).await?;
        let code = if error.contains("PAT_REQUIRED") {
            "PAT_REQUIRED"
        } else if error.contains("REVISION_CONFLICT") {
            "REVISION_CONFLICT"
        } else if error.contains("SOURCE_CHANGED") {
            "SOURCE_CHANGED"
        } else if error.contains("OPERATION_CONFLICT") {
            "OPERATION_CONFLICT"
        } else {
            "DISCOVERY_FAILED"
        };
        update_stage(&mut tx,fence,stage,json!({"status":if code=="PAT_REQUIRED"{"BLOCKED"}else{"FAILED"},"errorCode":code,"message":"Discovery did not commit this stage. Check access, upstream configuration and source revisions before retrying.","retryable":true,"completedAt":Utc::now()}),None).await?;
        tx.commit().await.map_err(db_error)
    }
    async fn interrupt(&self, fence: &DiscoveryFence) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let mut row=sqlx::query_scalar::<_,Json<Value>>("SELECT stage_results FROM channel_operation_receipts WHERE operation_id=$1 AND attempt=$2 AND state='RUNNING' FOR UPDATE").bind(fence.operation_id).bind(fence.attempt).fetch_optional(&mut *tx).await.map_err(db_error)?;
        if let Some(ref mut data) = row {
            for stage in ["models", "prices"] {
                if data.0[stage]["status"] == "RUNNING" {
                    data.0[stage] = json!({"status":"INTERRUPTED","errorCode":"DEADLINE_EXCEEDED","message":"The discovery deadline expired. Retry only this unfinished stage.","retryable":true});
                }
            }
            sqlx::query("UPDATE channel_operation_receipts SET stage_results=$3,state='INTERRUPTED',lease_expires_at=NULL,updated_at=now() WHERE operation_id=$1 AND attempt=$2").bind(fence.operation_id).bind(fence.attempt).bind(&data.0).execute(&mut *tx).await.map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)
    }
}
fn db_error(_: sqlx::Error) -> String {
    "STORAGE_FAILED: channel discovery could not persist its result".into()
}
fn op(id: &ID) -> Result<Uuid, String> {
    Uuid::parse_str(id.as_str())
        .map_err(|_| "OPERATION_CONFLICT: operationID must be a UUID".into())
}
fn numeric(id: &ID, expected: &str) -> Result<i64, String> {
    if let Ok(v) = id.as_str().parse() {
        return Ok(v);
    }
    let gid = conduit_admin_graphql::node::parse_guid(id.as_str())
        .map_err(|_| "INVALID_INPUT: invalid ID")?;
    if gid.typ != expected {
        return Err("INVALID_INPUT: wrong ID type".into());
    }
    Ok(gid.id)
}
fn integer(n: i64) -> Result<i32, String> {
    i32::try_from(n).map_err(|_| "INVALID_INPUT: discovery count exceeds GraphQL Int".into())
}
fn fingerprint(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn stage_key(stage: ChannelDiscoveryStage) -> &'static str {
    match stage {
        ChannelDiscoveryStage::Models => "models",
        ChannelDiscoveryStage::Prices => "prices",
    }
}
pub(crate) fn initial_results(
    stages: &[ChannelDiscoveryStage],
    rebuild: Option<&RebuildObservedPricePackagesInput>,
) -> Value {
    json!({"request":{"requestedStages":stages,"rebuild":rebuild,"discoveryRequestFingerprint":fingerprint(&json!({"stages":stages,"rebuild":rebuild}))},"models":{"status":if stages.contains(&ChannelDiscoveryStage::Models){"NOT_STARTED"}else{"SKIPPED"}},"prices":{"status":if stages.contains(&ChannelDiscoveryStage::Prices){"NOT_STARTED"}else{"SKIPPED"}},"packages":[],"supersededSourceIDs":[]})
}
fn normalized_stages(
    mut stages: Vec<ChannelDiscoveryStage>,
) -> Result<Vec<ChannelDiscoveryStage>, String> {
    stages.sort_by_key(|s| stage_key(*s));
    stages.dedup();
    if stages.is_empty() {
        return Err("INVALID_INPUT: select a discovery stage".into());
    }
    Ok(stages)
}
#[async_trait]
impl ChannelDiscoveryServices for PgChannelDiscoveryAdapter {
    async fn get(&self, operation: ID) -> Result<Option<ChannelDiscoveryOperation>, String> {
        match self.receipt(op(&operation)?).await? {
            Some(r) => self.dto(r).await.map(Some),
            None => Ok(None),
        }
    }
    async fn initialize(
        &self,
        actor: i64,
        can_rebuild: bool,
        channel: ID,
        operation: ID,
        stages: Vec<ChannelDiscoveryStage>,
        rebuild: Option<RebuildObservedPricePackagesInput>,
    ) -> Result<ChannelDiscoveryOperation, String> {
        if rebuild.is_some() && !can_rebuild {
            return Err(
                "FORBIDDEN: rebuilding price packages requires write_commercialization".into(),
            );
        }
        let stages = normalized_stages(stages)?;
        if rebuild.is_some() && !stages.contains(&ChannelDiscoveryStage::Prices) {
            return Err("INVALID_INPUT: rebuilding requires PRICES".into());
        }
        let channel = numeric(&channel, "Channel")?;
        let operation = op(&operation)?;
        let request = initial_results(&stages, rebuild.as_ref());
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("channel-operation:{operation}"))
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let valid=sqlx::query_scalar::<_,bool>("SELECT \"type\" IN ('openai','openai_responses') AND settings->>'managementAdapter'='new_api' FROM channels WHERE id=$1 AND deleted_at=0").bind(channel).fetch_optional(&mut *tx).await.map_err(db_error)?.unwrap_or(false);
        if !valid {
            return Err(
                "INVALID_INPUT: discovery requires an explicitly configured NEWAPI channel".into(),
            );
        }
        sqlx::query("INSERT INTO channel_operation_receipts(operation_id,channel_id,operation_type,origin,created_by,request_fingerprint,state,stage_results,created_at,updated_at) VALUES($1,$2,'NEW_API_DISCOVERY','USER_SYNC',$3,$4,'NOT_STARTED',$5,now(),now()) ON CONFLICT(operation_id) DO NOTHING").bind(operation).bind(channel).bind(actor).bind(fingerprint(&json!({"channel":channel,"request":request["request"]}))).bind(Json(&request)).execute(&mut *tx).await.map_err(db_error)?;
        let row=sqlx::query_as::<_,Receipt>(sqlx::AssertSqlSafe::<&str>(&format!("SELECT {RECEIPT_COLUMNS} FROM channel_operation_receipts WHERE operation_id=$1 AND operation_type='NEW_API_DISCOVERY' FOR UPDATE"))).bind(operation).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or("OPERATION_CONFLICT: operation type differs")?;
        if row.created_by != Some(actor)
            || row.channel_id != channel
            || row.stage_results.0["request"] != request["request"]
        {
            return Err("OPERATION_CONFLICT: discovery intent differs".into());
        }
        if row.state != "NOT_STARTED" {
            tx.commit().await.map_err(db_error)?;
            return self.dto(row).await;
        }
        let fence = claim(&mut tx, &row, &stages).await?;
        tx.commit().await.map_err(db_error)?;
        self.run_claimed(actor, fence, stages).await
    }
    async fn retry(
        &self,
        actor: i64,
        can_rebuild: bool,
        operation: ID,
        expected: i32,
        stages: Vec<ChannelDiscoveryStage>,
        credentials: Option<NewApiAccountCredentialsInput>,
    ) -> Result<ChannelDiscoveryOperation, String> {
        let operation = op(&operation)?;
        let stages = normalized_stages(stages)?;
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let row=sqlx::query_as::<_,Receipt>(sqlx::AssertSqlSafe::<&str>(&format!("SELECT {RECEIPT_COLUMNS} FROM channel_operation_receipts WHERE operation_id=$1 AND operation_type='NEW_API_DISCOVERY' FOR UPDATE"))).bind(operation).fetch_optional(&mut *tx).await.map_err(db_error)?.ok_or("OPERATION_CONFLICT: operation missing")?;
        // A retry resumes the creator's intent. It must not borrow that user's
        // publication authority or replace credentials on another user's receipt.
        if row.created_by != Some(actor) {
            return Err("FORBIDDEN: only the discovery creator may retry this operation".into());
        }
        if row
            .stage_results
            .0
            .pointer("/request/rebuild")
            .is_some_and(|v| !v.is_null())
            && !can_rebuild
        {
            return Err(
                "FORBIDDEN: retrying a price package rebuild requires write_commercialization"
                    .into(),
            );
        }
        if row.attempt != expected {
            return Err("REVISION_CONFLICT: reload the latest discovery attempt".into());
        }
        if row.state == "RUNNING" && row.lease_expires_at.is_some_and(|t| t > Utc::now()) {
            return Err("STATE_CHANGED: discovery is still running".into());
        }
        for stage in &stages {
            let status = row.stage_results.0[stage_key(*stage)]["status"]
                .as_str()
                .unwrap_or_default();
            if !matches!(status, "FAILED" | "BLOCKED" | "INTERRUPTED" | "RUNNING") {
                return Err(
                    "STATE_CHANGED: only failed or interrupted requested stages may be retried"
                        .into(),
                );
            }
        }
        if let Some(credentials) = credentials {
            if credentials.pat.trim().is_empty() {
                return Err("INVALID_INPUT: PAT must not be empty".into());
            }
            if credentials
                .user_id
                .as_ref()
                .is_some_and(|id| id.parse::<i64>().map_or(true, |id| id <= 0))
            {
                return Err("INVALID_INPUT: NEWAPI userID must be a positive integer".into());
            }
            let mut patch = json!({"newApiPat":credentials.pat});
            if let Some(id) = credentials.user_id {
                patch["newApiUserID"] = json!(id)
            }
            sqlx::query("UPDATE channels SET credentials=credentials||$2::jsonb,updated_at=now() WHERE id=$1 AND deleted_at=0").bind(row.channel_id).bind(patch).execute(&mut *tx).await.map_err(db_error)?;
        }
        let fence = claim(&mut tx, &row, &stages).await?;
        tx.commit().await.map_err(db_error)?;
        self.run_claimed(actor, fence, stages).await
    }
}
async fn claim(
    tx: &mut Transaction<'_, Postgres>,
    row: &Receipt,
    stages: &[ChannelDiscoveryStage],
) -> Result<DiscoveryFence, String> {
    let mut data = row.stage_results.0.clone();
    for stage in stages {
        data[stage_key(*stage)] = json!({"status":"RUNNING","retryable":false});
    }
    let attempt = row
        .attempt
        .checked_add(1)
        .ok_or("INVALID_INPUT: attempt overflow")?;
    sqlx::query("UPDATE channel_operation_receipts SET attempt=$2,state='RUNNING',stage_results=$3,lease_expires_at=now()+interval '90 seconds',updated_at=now() WHERE operation_id=$1").bind(row.operation_id).bind(attempt).bind(Json(data)).execute(&mut **tx).await.map_err(db_error)?;
    Ok(DiscoveryFence {
        operation_id: row.operation_id,
        attempt,
    })
}
pub(crate) async fn lock_fence(
    tx: &mut Transaction<'_, Postgres>,
    fence: &DiscoveryFence,
) -> Result<(), String> {
    let valid=sqlx::query_scalar::<_,bool>("SELECT attempt=$2 AND state='RUNNING' AND lease_expires_at>clock_timestamp() FROM channel_operation_receipts WHERE operation_id=$1 AND operation_type='NEW_API_DISCOVERY' FOR UPDATE").bind(fence.operation_id).bind(fence.attempt).fetch_optional(&mut **tx).await.map_err(db_error)?.unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err("STATE_CHANGED: discovery attempt no longer owns the lease".into())
    }
}
async fn update_stage(
    tx: &mut Transaction<'_, Postgres>,
    fence: &DiscoveryFence,
    stage: ChannelDiscoveryStage,
    result: Value,
    package: Option<&StagedPackage>,
) -> Result<(), String> {
    lock_fence(tx, fence).await?;
    let mut data = sqlx::query_scalar::<_, Json<Value>>(
        "SELECT stage_results FROM channel_operation_receipts WHERE operation_id=$1",
    )
    .bind(fence.operation_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(db_error)?
    .0;
    data[stage_key(stage)] = result;
    if let Some(p) = package {
        data["packages"]
            .as_array_mut()
            .ok_or("STORAGE_FAILED: invalid receipt")?
            .push(json!({"id":p.id,"reused":p.reused}));
    }
    let statuses = [
        data["models"]["status"].as_str().unwrap_or_default(),
        data["prices"]["status"].as_str().unwrap_or_default(),
    ];
    let state = if statuses.contains(&"RUNNING") {
        "RUNNING"
    } else if statuses
        .iter()
        .all(|s| matches!(*s, "SUCCEEDED" | "NO_CHANGE" | "SKIPPED"))
    {
        if data["needsAction"].as_bool().unwrap_or(false) {
            "NEEDS_ACTION"
        } else {
            "SUCCEEDED"
        }
    } else if statuses
        .iter()
        .any(|s| matches!(*s, "SUCCEEDED" | "NO_CHANGE"))
    {
        "PARTIAL"
    } else {
        "FAILED"
    };
    sqlx::query("UPDATE channel_operation_receipts SET stage_results=$3,state=$4,lease_expires_at=CASE WHEN $4='RUNNING' THEN lease_expires_at ELSE NULL END,updated_at=now() WHERE operation_id=$1 AND attempt=$2").bind(fence.operation_id).bind(fence.attempt).bind(Json(data)).bind(state).execute(&mut **tx).await.map_err(db_error)?;
    Ok(())
}
pub(crate) async fn finish_models(
    tx: &mut Transaction<'_, Postgres>,
    fence: &DiscoveryFence,
    count: usize,
    package: Option<&StagedPackage>,
) -> Result<(), String> {
    finish_stage(
        tx,
        fence,
        ChannelDiscoveryStage::Models,
        count,
        None,
        package,
    )
    .await
}
pub(crate) async fn finish_prices(
    tx: &mut Transaction<'_, Postgres>,
    fence: &DiscoveryFence,
    channel: i64,
    commit: &crate::wiring_postgres_provider_pricing::PricingDiscoveryCommit,
    snapshot: &crate::wiring_postgres_provider_quota::NewApiPricingSnapshot,
    rebuild: Option<&RebuildObservedPricePackagesInput>,
) -> Result<(), String> {
    lock_fence(tx, fence).await?;
    if let Some(rebuild) = rebuild {
        let mut sources = rebuild
            .sources
            .iter()
            .map(|source| {
                numeric(&source.id, "ChangeSet").map(|id| (id, source.expected_revision.as_str()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        sources.sort_by_key(|(id, _)| *id);
        if sources.is_empty() || sources.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err("INVALID_INPUT: choose distinct source packages".into());
        }
        let mut eligible = Vec::new();
        for (id, expected) in &sources {
            let (kind,scope,status,revision)=sqlx::query_as::<_,(String,String,String,String)>("SELECT kind,scope_id,status,source_revision FROM change_sets WHERE id=$1 AND scope_type='channel' FOR UPDATE").bind(id).fetch_optional(&mut **tx).await.map_err(db_error)?.ok_or("SOURCE_CHANGED: source package not found")?;
            if kind != "provider_price"
                || scope != channel.to_string()
                || !matches!(status.as_str(), "pending_review" | "invalid" | "superseded")
            {
                return Err(
                    "SOURCE_CHANGED: source is not an eligible observed package in this channel"
                        .into(),
                );
            }
            let current = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(id),0) FROM change_set_events WHERE change_set_id=$1",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
            if current.to_string() != *expected {
                return Err("REVISION_CONFLICT: selected source package changed".into());
            }
            let (count,proved)=sqlx::query_as::<_,(i64,bool)>("SELECT COUNT(*),COALESCE(bool_and(i.source_snapshot->>'source'='observed' AND (i.source_snapshot->>'snapshotID')= $2 AND EXISTS(SELECT 1 FROM provider_price_rows r JOIN provider_price_snapshots s ON s.id=r.snapshot_id WHERE r.id::text=i.source_snapshot->>'providerPriceRowID' AND r.snapshot_id::text=i.source_snapshot->>'snapshotID' AND r.channel_id=$3 AND s.channel_id=$3 AND s.status='success' AND r.upstream_model_id=i.item_key)),false) FROM change_set_items i WHERE change_set_id=$1").bind(id).bind(&revision).bind(channel).fetch_one(&mut **tx).await.map_err(db_error)?;
            let manual=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM change_set_events WHERE change_set_id=$1 AND detail->>'origin' IN('MANUAL_EDIT','CORRECTION','CHANNEL_IMPORT'))").bind(id).fetch_one(&mut **tx).await.map_err(db_error)?;
            if count == 0 || !proved || manual {
                return Err(
                    "SOURCE_CHANGED: selected package lacks verified observed pricing provenance"
                        .into(),
                );
            }
            if status != "superseded" {
                eligible.push(*id);
            }
        }
        if let Some(package) = &commit.package {
            sqlx::query("INSERT INTO change_set_events(change_set_id,event_type,actor_type,actor_id,detail,created_at) SELECT $1,'derived_from','user',created_by,$3,now() FROM channel_operation_receipts WHERE operation_id=$2").bind(package.id).bind(fence.operation_id).bind(json!({"derivedFromChangeSetIDs":sources.iter().map(|(id,_)|id).collect::<Vec<_>>() })).execute(&mut **tx).await.map_err(db_error)?;
        }
        if rebuild.supersede_sources {
            for id in &eligible {
                sqlx::query("UPDATE change_sets SET status='superseded',updated_at=now(),reviewed_at=now(),review_note='explicitly rebuilt from a complete fresh observation',validation_error=NULL WHERE id=$1").bind(id).execute(&mut **tx).await.map_err(db_error)?;
                sqlx::query("INSERT INTO change_set_events(change_set_id,event_type,actor_type,actor_id,detail,created_at) SELECT $1,'superseded','user',created_by,$3,now() FROM channel_operation_receipts WHERE operation_id=$2").bind(id).bind(fence.operation_id).bind(json!({"reason":"explicit_rebuild","replacementChangeSetID":commit.package.as_ref().map(|p|p.id),"replacementOperationID":fence.operation_id,"snapshotID":commit.snapshot_id})).execute(&mut **tx).await.map_err(db_error)?;
            }
            sqlx::query("UPDATE channel_operation_receipts SET stage_results=jsonb_set(stage_results,'{supersededSourceIDs}',$2::jsonb) WHERE operation_id=$1").bind(fence.operation_id).bind(json!(eligible)).execute(&mut **tx).await.map_err(db_error)?;
        }
    }
    finish_stage(
        tx,
        fence,
        ChannelDiscoveryStage::Prices,
        snapshot.models.len(),
        Some(commit.snapshot_id),
        commit.package.as_ref(),
    )
    .await
}
async fn finish_stage(
    tx: &mut Transaction<'_, Postgres>,
    fence: &DiscoveryFence,
    stage: ChannelDiscoveryStage,
    count: usize,
    snapshot: Option<i64>,
    package: Option<&StagedPackage>,
) -> Result<(), String> {
    if package.is_some_and(|p| p.invalid_count > 0) {
        sqlx::query("UPDATE channel_operation_receipts SET stage_results=jsonb_set(stage_results,'{needsAction}','true'::jsonb) WHERE operation_id=$1 AND attempt=$2").bind(fence.operation_id).bind(fence.attempt).execute(&mut **tx).await.map_err(db_error)?;
    }
    update_stage(tx,fence,stage,json!({"status":if stage==ChannelDiscoveryStage::Prices&&package.is_none(){"NO_CHANGE"}else{"SUCCEEDED"},"discoveredCount":count,"changedCount":package.map(|p|p.item_count).unwrap_or(0),"unsupportedCount":package.map(|p|p.invalid_count).unwrap_or(0),"snapshotID":snapshot,"retryable":false,"completedAt":Utc::now()}),package).await
}
fn stage_dto(v: &Value, expired: bool) -> ChannelDiscoveryStageResult {
    let mut status = serde_json::from_value::<ChannelDiscoveryStageStatus>(v["status"].clone())
        .unwrap_or(ChannelDiscoveryStageStatus::NotStarted);
    if expired && status == ChannelDiscoveryStageStatus::Running {
        status = ChannelDiscoveryStageStatus::Interrupted;
    }
    ChannelDiscoveryStageResult {
        status,
        discovered_count: v["discoveredCount"]
            .as_i64()
            .and_then(|n| n.try_into().ok())
            .unwrap_or(0),
        changed_count: v["changedCount"]
            .as_i64()
            .and_then(|n| n.try_into().ok())
            .unwrap_or(0),
        unsupported_count: v["unsupportedCount"]
            .as_i64()
            .and_then(|n| n.try_into().ok())
            .unwrap_or(0),
        error_code: v["errorCode"].as_str().map(str::to_string),
        message: v["message"].as_str().map(str::to_string),
        retryable: status == ChannelDiscoveryStageStatus::Interrupted
            || v["retryable"].as_bool().unwrap_or(false),
        snapshot_id: v["snapshotID"].as_i64().map(|id| ID(id.to_string())),
        completed_at: v["completedAt"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| TimeScalar(d.with_timezone(&Utc))),
    }
}
fn kind_from(s: &str) -> Result<ChangeSetKind, String> {
    match s {
        "provider_price" => Ok(ChangeSetKind::ProviderPrice),
        "model_mapping" => Ok(ChangeSetKind::ModelMapping),
        "retail_price" => Ok(ChangeSetKind::RetailPrice),
        "model_supply" => Ok(ChangeSetKind::ModelSupply),
        _ => Err("STORAGE_FAILED: invalid package kind".into()),
    }
}
fn status_from(s: &str) -> Result<ChangeSetStatus, String> {
    match s {
        "draft" => Ok(ChangeSetStatus::Draft),
        "pending_review" => Ok(ChangeSetStatus::PendingReview),
        "invalid" => Ok(ChangeSetStatus::Invalid),
        "applied" => Ok(ChangeSetStatus::Applied),
        "approved" => Ok(ChangeSetStatus::Approved),
        "rejected" => Ok(ChangeSetStatus::Rejected),
        "superseded" => Ok(ChangeSetStatus::Superseded),
        _ => Err("STORAGE_FAILED: invalid package status".into()),
    }
}

#[cfg(test)]
#[path = "wiring_postgres_channel_discovery_retry_tests.rs"]
mod retry_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn r3_discovery_is_durable_partial_and_retries_only_failed_stage()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/api/status")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"data":{"version":"c2b7a9a","quota_per_unit":500000,"quota_display_type":"USD"}}))).mount(&server).await;
        Mock::given(method("GET")).and(path("/api/pricing")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"success":true,"data":[{"model_name":"m","quota_type":0,"model_ratio":1,"completion_ratio":2,"billing_mode":"legacy"}],"group_ratio":{"default":1}}))).mount(&server).await;
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO channels(\"type\",name,status,base_url,credentials,supported_models,default_test_model,settings) VALUES('openai','discovery-test','enabled',$1,'{\"apiKeys\":[\"test-key\"]}','[]','', '{\"managementAdapter\":\"new_api\",\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}') RETURNING id").bind(server.uri()).fetch_one(&db.pool).await?;
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(db.pool.clone())),
            Arc::new(conduit_cache::NoopCache::new()),
        ));
        let adapter = PgChannelDiscoveryAdapter::new(db.pool.clone(), system, false);
        let op = ID(Uuid::new_v4().to_string());
        let first = adapter
            .initialize(
                7,
                false,
                ID(id.to_string()),
                op.clone(),
                vec![ChannelDiscoveryStage::Models, ChannelDiscoveryStage::Prices],
                None,
            )
            .await?;
        assert_eq!(first.models.status, ChannelDiscoveryStageStatus::Failed);
        assert!(first.models.retryable);
        assert!(!first.prices.retryable);
        assert_eq!(first.state, ChannelDiscoveryState::Partial);
        assert_eq!(first.packages.len(), 1);
        assert_eq!(first.package_validation.invalid_items, 1);
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests
                .iter()
                .filter(|r| r.url.path().starts_with("/api/"))
                .all(|r| !r.headers.contains_key("authorization")),
            "API KEY is not sent to management endpoints"
        );
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"m"}]})))
            .expect(1)
            .mount(&server)
            .await;
        let retried = adapter
            .retry(
                7,
                false,
                op.clone(),
                first.attempt,
                vec![ChannelDiscoveryStage::Models],
                None,
            )
            .await?;
        assert_eq!(retried.attempt, 2);
        assert_eq!(
            retried.models.status,
            ChannelDiscoveryStageStatus::Succeeded
        );
        assert_eq!(retried.state, ChannelDiscoveryState::NeedsAction);
        assert_eq!(retried.prices.snapshot_id, first.prices.snapshot_id);
        assert_eq!(retried.packages.len(), 2);
        assert!(
            retried
                .packages
                .iter()
                .any(|p| p.kind == ChangeSetKind::ModelSupply
                    && p.status == ChangeSetStatus::PendingReview)
        );
        assert_eq!(
            sqlx::query_scalar::<_, Json<Value>>(
                "SELECT supported_models FROM channels WHERE id=$1"
            )
            .bind(id)
            .fetch_one(&db.pool)
            .await?
            .0,
            json!([]),
            "initialization observations must not publish online supply"
        );
        let again = adapter
            .initialize(
                7,
                false,
                ID(id.to_string()),
                op.clone(),
                vec![ChannelDiscoveryStage::Models, ChannelDiscoveryStage::Prices],
                None,
            )
            .await?;
        assert_eq!(again.attempt, 2);
        assert!(
            adapter
                .retry(7, false, op, 1, vec![ChannelDiscoveryStage::Prices], None)
                .await
                .is_err()
        );
        server.verify().await;
        db.cleanup().await?;
        Ok(())
    }
    #[tokio::test]
    async fn r3_rebuild_checks_source_revision_permissions_and_only_supersedes_selected_observations()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel=sqlx::query_scalar::<_,i64>("INSERT INTO channels(\"type\",name,status,credentials,supported_models,default_test_model,settings) VALUES('openai','rebuild-test','enabled','{}','[]','', '{\"managementAdapter\":\"new_api\"}') RETURNING id").fetch_one(&db.pool).await?;
        let snapshot_id=sqlx::query_scalar::<_,i64>("INSERT INTO provider_price_snapshots(channel_id,adapter_id,adapter_version,attempted_endpoints,status,warnings,started_at,observed_at) VALUES($1,'new_api','test','[]','success','[]',now(),now()) RETURNING id").bind(channel).fetch_one(&db.pool).await?;
        let row_id=sqlx::query_scalar::<_,i64>("INSERT INTO provider_price_rows(snapshot_id,channel_id,upstream_model_id,group_name,billing_kind,quality) VALUES($1,$2,'m','','token','exact') RETURNING id").bind(snapshot_id).bind(channel).fetch_one(&db.pool).await?;
        let mut packages = Vec::new();
        for origin in ["AUTOMATIC_SYNC", "USER_SYNC", "MANUAL_EDIT"] {
            let id=sqlx::query_scalar::<_,i64>("INSERT INTO change_sets(kind,scope_type,scope_id,title,status,source_revision,created_at,updated_at) VALUES('provider_price','channel',$1,$2,'pending_review',$3,now(),now()) RETURNING id").bind(channel.to_string()).bind(origin).bind(snapshot_id.to_string()).fetch_one(&db.pool).await?;
            sqlx::query("INSERT INTO change_set_items(change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) VALUES($1,'m','create','{}',$2,now(),now())").bind(id).bind(Json(json!({"source":"observed","snapshotID":snapshot_id,"providerPriceRowID":row_id}))).execute(&db.pool).await?;
            let revision=sqlx::query_scalar::<_,i64>("INSERT INTO change_set_events(change_set_id,event_type,actor_type,actor_id,detail,created_at) VALUES($1,'created','user',7,$2,now()) RETURNING id").bind(id).bind(Json(json!({"origin":origin}))).fetch_one(&db.pool).await?;
            packages.push((id, revision.to_string()));
        }
        let operation = Uuid::new_v4();
        let selected = RebuildObservedPricePackagesInput {
            sources: vec![ChangeSetRevisionInput {
                id: ID(packages[0].0.to_string()),
                expected_revision: packages[0].1.clone(),
            }],
            supersede_sources: true,
        };
        let mut initial = initial_results(&[ChannelDiscoveryStage::Prices], Some(&selected));
        initial["prices"] = json!({"status":"FAILED","retryable":true});
        sqlx::query("INSERT INTO channel_operation_receipts(operation_id,channel_id,operation_type,origin,created_by,request_fingerprint,attempt,state,stage_results,created_at,updated_at) VALUES($1,$2,'NEW_API_DISCOVERY','USER_SYNC',7,'test',1,'FAILED',$3,now(),now())").bind(operation).bind(channel).bind(Json(initial)).execute(&db.pool).await?;
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(db.pool.clone())),
            Arc::new(conduit_cache::NoopCache::new()),
        ));
        let adapter = PgChannelDiscoveryAdapter::new(db.pool.clone(), system, false);
        assert!(
            adapter
                .initialize(
                    7,
                    false,
                    ID(channel.to_string()),
                    ID(operation.to_string()),
                    vec![ChannelDiscoveryStage::Prices],
                    Some(selected.clone())
                )
                .await
                .unwrap_err()
                .starts_with("FORBIDDEN")
        );
        assert!(
            adapter
                .retry(
                    7,
                    false,
                    ID(operation.to_string()),
                    1,
                    vec![ChannelDiscoveryStage::Prices],
                    None
                )
                .await
                .unwrap_err()
                .starts_with("FORBIDDEN")
        );
        sqlx::query("UPDATE channel_operation_receipts SET state='RUNNING',lease_expires_at=now()+interval '90 seconds',stage_results=jsonb_set(stage_results,'{prices}','{\"status\":\"RUNNING\"}') WHERE operation_id=$1").bind(operation).execute(&db.pool).await?;
        let fence = DiscoveryFence {
            operation_id: operation,
            attempt: 1,
        };
        let snapshot = crate::wiring_postgres_provider_quota::NewApiPricingSnapshot {
            fetched_at: Utc::now(),
            source_endpoint: "/api/pricing".into(),
            pricing_version: None,
            account_group: None,
            effective_groups: Vec::new(),
            key_count: 0,
            matched_key_count: 0,
            warnings: Vec::new(),
            models: Vec::new(),
        };
        let commit = crate::wiring_postgres_provider_pricing::PricingDiscoveryCommit {
            snapshot_id,
            package: None,
        };
        let mut wrong = selected.clone();
        wrong.sources[0].expected_revision = "old".into();
        let mut tx = db.pool.begin().await?;
        assert!(
            finish_prices(&mut tx, &fence, channel, &commit, &snapshot, Some(&wrong))
                .await
                .unwrap_err()
                .starts_with("REVISION_CONFLICT")
        );
        tx.rollback().await?;
        wrong.sources[0] = ChangeSetRevisionInput {
            id: ID(packages[2].0.to_string()),
            expected_revision: packages[2].1.clone(),
        };
        let mut tx = db.pool.begin().await?;
        assert!(
            finish_prices(&mut tx, &fence, channel, &commit, &snapshot, Some(&wrong))
                .await
                .unwrap_err()
                .contains("verified observed")
        );
        tx.rollback().await?;
        let replacement=sqlx::query_scalar::<_,i64>("INSERT INTO change_sets(operation_id,kind,scope_type,scope_id,title,status,source_revision,created_at,updated_at) VALUES($1,'provider_price','channel',$2,'fresh replacement','pending_review',$3,now(),now()) RETURNING id").bind(Uuid::new_v4()).bind(channel.to_string()).bind(snapshot_id.to_string()).fetch_one(&db.pool).await?;
        sqlx::query("INSERT INTO change_set_events(change_set_id,event_type,actor_type,actor_id,detail,created_at) VALUES($1,'created','user',7,'{}',now())").bind(replacement).execute(&db.pool).await?;
        let commit = crate::wiring_postgres_provider_pricing::PricingDiscoveryCommit {
            snapshot_id,
            package: Some(StagedPackage {
                id: replacement,
                reused: false,
                item_count: 1,
                invalid_count: 0,
            }),
        };
        let mut tx = db.pool.begin().await?;
        finish_prices(
            &mut tx,
            &fence,
            channel,
            &commit,
            &snapshot,
            Some(&selected),
        )
        .await?;
        tx.commit().await?;
        for (index, (id, _)) in packages.iter().enumerate() {
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
                    .bind(id)
                    .fetch_one(&db.pool)
                    .await?,
                if index == 0 {
                    "superseded"
                } else {
                    "pending_review"
                }
            );
        }
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM change_set_events WHERE change_set_id=$1 AND event_type='derived_from'").bind(replacement).fetch_one(&db.pool).await?,1,"lineage must append a new event without changing existing events");
        let result = adapter.get(ID(operation.to_string())).await?.unwrap();
        assert_eq!(
            result.superseded_source_change_set_ids,
            vec![ID(format!("gid://conduit/ChangeSet/{}", packages[0].0))]
        );
        assert_eq!(result.state, ChannelDiscoveryState::Succeeded);
        db.cleanup().await?;
        Ok(())
    }
}

//! Actual retry entry tests: no provider credentials or user database required.
use super::*;
use axum::{Json as HttpJson, Router, extract::State, http::StatusCode};
use conduit_admin_graphql::{
    governance::GovernanceServices as _, governance::UpdateGovernanceSettingsInput,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;

struct StubState {
    failing: AtomicBool,
    requests: AtomicUsize,
    gate_enabled: AtomicBool,
    gate_path: &'static str,
    entered: Notify,
    release: Notify,
}
struct Stub {
    uri: String,
    state: Arc<StubState>,
    task: tokio::task::JoinHandle<()>,
}
impl Stub {
    async fn start(stage: ChannelDiscoveryStage) -> Result<Self, std::io::Error> {
        let state = Arc::new(StubState {
            failing: AtomicBool::new(true),
            requests: AtomicUsize::new(0),
            gate_enabled: AtomicBool::new(false),
            gate_path: if stage == ChannelDiscoveryStage::Prices {
                "/api/pricing"
            } else {
                "/v1/models"
            },
            entered: Notify::new(),
            release: Notify::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let uri = format!("http://{}", listener.local_addr()?);
        let router = Router::new()
            .fallback(stub_response)
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Ok(Self { uri, state, task })
    }
    async fn stop(&self) {
        self.task.abort();
        while !self.task.is_finished() {
            tokio::task::yield_now().await;
        }
    }
}
impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn stub_response(
    State(state): State<Arc<StubState>>,
    uri: axum::http::Uri,
) -> (StatusCode, HttpJson<Value>) {
    state.requests.fetch_add(1, Ordering::SeqCst);
    if state.failing.load(Ordering::SeqCst) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            HttpJson(json!({"success":false})),
        );
    }
    if state.gate_enabled.load(Ordering::SeqCst) && uri.path() == state.gate_path {
        state.entered.notify_one();
        state.release.notified().await;
    }
    let body = match uri.path() {
        "/v1/models" => json!({"data":[{"id":"m"}]}),
        "/api/status" => {
            json!({"success":true,"data":{"version":"c2b7a9a","quota_per_unit":500000,"quota_display_type":"USD"}})
        }
        "/api/user/self" => json!({"success":true,"data":{"group":"default"}}),
        "/api/token/" => {
            json!({"success":true,"data":{"total":1,"items":[{"key":"abcd**********5678","group":"default"}]}})
        }
        "/api/pricing" => {
            json!({"success":true,"pricing_version":"retry-v1","group_ratio":{"default":1},"data":[{"model_name":"m","quota_type":0,"model_ratio":1,"completion_ratio":2,"billing_mode":"legacy","enable_groups":["all"]}]})
        }
        _ => return (StatusCode::NOT_FOUND, HttpJson(json!({"success":false}))),
    };
    (StatusCode::OK, HttpJson(body))
}
async fn fixture(pool: &PgPool) -> Result<(i64, i64), Box<dyn std::error::Error>> {
    let admin:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,is_owner) VALUES('retry-owner@fixture.invalid','fixture','activated',true) RETURNING id").fetch_one(pool).await?;
    crate::wiring_governance::PgGovernanceAdapter::new(pool.clone())
        .update_settings(
            admin,
            UpdateGovernanceSettingsInput {
                enabled: false,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let creator=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('retry-creator@fixture.invalid','fixture','activated','[\"write_channels\",\"write_commercialization\"]') RETURNING id").fetch_one(pool).await?;
    let other=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('retry-other@fixture.invalid','fixture','activated','[\"write_channels\"]') RETURNING id").fetch_one(pool).await?;
    Ok((creator, other))
}
async fn channel(pool: &PgPool, stub: &Stub) -> Result<i64, sqlx::Error> {
    let public_id = format!("retry-{}", Uuid::new_v4());
    sqlx::query_scalar("INSERT INTO channels(type,name,status,base_url,credentials,supported_models,default_test_model,settings) VALUES('openai',$4,'disabled',$1,$2,'[]','',$3) RETURNING id")
        .bind(&stub.uri)
        .bind(Json(json!({"apiKeys":["abcd01234567895678"],"newApiPat":"user-pat","newApiUserID":"19301"})))
        .bind(Json(json!({"managementAdapter":"new_api","billingCurrency":"CNY","rechargeMultiplier":"1","autoModelMappingRules":[{"pattern":"^m$","publicModelIdTemplate":public_id,"createDraft":true,"developerTemplate":"fixture","modelType":"chat"}]})))
        .bind(format!("retry-{}", Uuid::new_v4()))
        .fetch_one(pool).await
}
fn adapter(pool: &PgPool) -> Arc<PgChannelDiscoveryAdapter> {
    let system = Arc::new(SystemService::from_system_repo(
        Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
        Arc::new(conduit_cache::NoopCache::new()),
    ));
    Arc::new(PgChannelDiscoveryAdapter::new(pool.clone(), system, false))
}
async fn formal(pool: &PgPool, channel: i64) -> Result<(i64, i64, Value), sqlx::Error> {
    let prices =
        sqlx::query_scalar("SELECT COUNT(*) FROM channel_model_price_versions WHERE channel_id=$1")
            .bind(channel)
            .fetch_one(pool)
            .await?;
    let routes=sqlx::query_scalar("SELECT COUNT(*) FROM model_routes r JOIN upstream_model_deployments d ON d.id=r.deployment_id WHERE d.channel_id=$1").bind(channel).fetch_one(pool).await?;
    let supported =
        sqlx::query_scalar::<_, Json<Value>>("SELECT supported_models FROM channels WHERE id=$1")
            .bind(channel)
            .fetch_one(pool)
            .await?
            .0;
    Ok((prices, routes, supported))
}

#[tokio::test]
async fn repair_retry_creator_boundary_covers_prices_models_and_expired_recovery()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    let (creator, other) = fixture(&db.pool).await?;
    let adapter = adapter(&db.pool);
    for stage in [ChannelDiscoveryStage::Prices, ChannelDiscoveryStage::Models] {
        for expired in [false, true] {
            let stub = Stub::start(stage).await?;
            let result=async {
                let channel=channel(&db.pool,&stub).await?;
                let operation=ID(Uuid::new_v4().to_string());
                let first=adapter.initialize(creator,false,ID(channel.to_string()),operation.clone(),vec![stage],None).await?;
                assert_eq!(if stage==ChannelDiscoveryStage::Prices {first.prices.status} else {first.models.status},ChannelDiscoveryStageStatus::Failed);
                if expired {
                    sqlx::query("UPDATE channel_operation_receipts SET state='RUNNING',lease_expires_at=now()-interval '1 second',stage_results=jsonb_set(stage_results,$2::text[],'{\"status\":\"RUNNING\"}') WHERE operation_id=$1")
                        .bind(op(&operation)?).bind(vec![stage_key(stage)]).execute(&db.pool).await?;
                    assert_eq!(adapter.get(operation.clone()).await?.unwrap().state,ChannelDiscoveryState::Interrupted);
                }
                let before=sqlx::query_scalar::<_,Json<Value>>("SELECT to_jsonb(r) FROM channel_operation_receipts r WHERE operation_id=$1").bind(op(&operation)?).fetch_one(&db.pool).await?.0;
                let before_credentials=sqlx::query_scalar::<_,Json<Value>>("SELECT credentials FROM channels WHERE id=$1").bind(channel).fetch_one(&db.pool).await?.0;
                let before_formal=formal(&db.pool,channel).await?;
                stub.state.failing.store(false,Ordering::SeqCst);
                let requests=stub.state.requests.load(Ordering::SeqCst);
                let error=adapter.retry(other,true,operation.clone(),first.attempt,vec![stage],Some(NewApiAccountCredentialsInput {pat:"other-fixture-pat".into(),user_id:Some("19302".into())})).await.unwrap_err();
                assert!(error.starts_with("FORBIDDEN: only the discovery creator"),"{error}");
                assert_eq!(stub.state.requests.load(Ordering::SeqCst),requests,"denied retry performs no upstream work");
                assert_eq!(sqlx::query_scalar::<_,Json<Value>>("SELECT to_jsonb(r) FROM channel_operation_receipts r WHERE operation_id=$1").bind(op(&operation)?).fetch_one(&db.pool).await?.0,before,"denied retry does not claim or rewrite audit");
                assert_eq!(sqlx::query_scalar::<_,Json<Value>>("SELECT credentials FROM channels WHERE id=$1").bind(channel).fetch_one(&db.pool).await?.0,before_credentials,"denied retry cannot replace channel credentials");
                assert_eq!(formal(&db.pool,channel).await?,before_formal);
                let legal=adapter.retry(creator,false,operation.clone(),first.attempt,vec![stage],None).await?;
                assert_eq!(legal.attempt,first.attempt+1);
                assert_eq!(legal.created_by,Some(ID(format!("gid://conduit/User/{creator}"))));
                assert_eq!(if stage==ChannelDiscoveryStage::Prices {legal.prices.status} else {legal.models.status},ChannelDiscoveryStageStatus::Succeeded);
                let after=formal(&db.pool,channel).await?;
                if stage==ChannelDiscoveryStage::Prices { assert_eq!(after.0,1,"creator price retry publishes"); } else { assert_eq!(after.1,1,"creator Models retry publishes mapping"); }
                assert_eq!(after.2,if stage==ChannelDiscoveryStage::Models {json!(["m"])} else {json!([])},"only the creator's authorized OFF supply action may update online models");
                assert!(adapter.retry(creator,false,operation.clone(),first.attempt,vec![stage],None).await.unwrap_err().starts_with("REVISION_CONFLICT"));
                // Missing historical authors are not interpreted as automatic producers.
                sqlx::query("UPDATE channel_operation_receipts SET created_by=NULL WHERE operation_id=$1").bind(op(&operation)?).execute(&db.pool).await?;
                assert!(adapter.retry(other,true,operation,legal.attempt,vec![stage],None).await.unwrap_err().starts_with("FORBIDDEN: only the discovery creator"));
                Ok::<(),Box<dyn std::error::Error>>(())
            }.await;
            stub.stop().await;
            result?;
        }
    }
    println!(
        "repair retry actual SQL: creator/B denied and creator allowed; Prices+Models, failed+expired, credentials/audit/fence/supply checked"
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn repair_retry_current_creator_revocation_before_prices_or_models_commit_blocks_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    let (creator, _) = fixture(&db.pool).await?;
    let adapter = adapter(&db.pool);
    for stage in [ChannelDiscoveryStage::Prices, ChannelDiscoveryStage::Models] {
        let stub = Stub::start(stage).await?;
        let result=async {
            sqlx::query("UPDATE users SET scopes='[\"write_channels\",\"write_commercialization\"]' WHERE id=$1").bind(creator).execute(&db.pool).await?;
            let channel=channel(&db.pool,&stub).await?;
            let operation=ID(Uuid::new_v4().to_string());
            let first=adapter.initialize(creator,false,ID(channel.to_string()),operation.clone(),vec![stage],None).await?;
            let before=formal(&db.pool,channel).await?;
            stub.state.failing.store(false,Ordering::SeqCst);
            stub.state.gate_enabled.store(true,Ordering::SeqCst);
            let task_adapter=adapter.clone();let task_operation=operation.clone();
            let retry=tokio::spawn(async move {task_adapter.retry(creator,false,task_operation,first.attempt,vec![stage],None).await});
            tokio::time::timeout(std::time::Duration::from_secs(10),stub.state.entered.notified()).await?;
            // The upstream response cannot complete until the real current actor's
            // permissions have been revoked and committed to PostgreSQL.
            sqlx::query("UPDATE users SET scopes='[\"write_channels\"]' WHERE id=$1").bind(creator).execute(&db.pool).await?;
            stub.state.release.notify_one();
            let completed=tokio::time::timeout(std::time::Duration::from_secs(10),retry).await???;
            assert_eq!(completed.created_by,Some(ID(format!("gid://conduit/User/{creator}"))));
            let after=formal(&db.pool,channel).await?;
            assert_eq!((after.0,after.1),(before.0,before.1),"revoked actual retry actor cannot publish Prices or mappings");
            assert_eq!(after.2,if stage==ChannelDiscoveryStage::Models {json!(["m"])} else {json!([])},"retained channel write authority still follows the existing OFF supply policy");
            let kind=if stage==ChannelDiscoveryStage::Prices {"provider_price"} else {"model_mapping"};
            let (status,author):(String,i64)=sqlx::query_as("SELECT status,created_by FROM change_sets WHERE operation_id=$1 AND kind=$2").bind(op(&operation)?).bind(kind).fetch_one(&db.pool).await?;
            assert_eq!(status,"pending_review");assert_eq!(author,creator);
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM change_set_events e JOIN change_sets c ON c.id=e.change_set_id WHERE c.operation_id=$1 AND c.kind=$2 AND e.event_type='publication_blocked' AND e.actor_id=$3").bind(op(&operation)?).bind(kind).bind(creator).fetch_one(&db.pool).await?,1);
            let count=if stage==ChannelDiscoveryStage::Prices {sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM provider_price_snapshots WHERE channel_id=$1").bind(channel).fetch_one(&db.pool).await?} else {sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM channel_model_observations WHERE channel_id=$1").bind(channel).fetch_one(&db.pool).await?};
            assert_eq!(count,1,"revocation preserves observation");
            Ok::<(),Box<dyn std::error::Error>>(())
        }.await;
        stub.stop().await;
        result?;
    }
    println!(
        "repair retry actual SQL: awaited response gate revocation checked for Prices+Models; observations retained, formal data unchanged"
    );
    db.cleanup().await?;
    Ok(())
}

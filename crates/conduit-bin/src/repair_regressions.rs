//! Fault and concurrency regressions for metering and consistent backup input.
use crate::usage_recovery::{MeteredEvent, UsageJournal, event_key, handoff};
use conduit_config::model::UsageRecoveryConfig;
use conduit_db::{PolicyContext, Principal, RequestContext, repo::usage_repo::CreateUsageLogInput};
use conduit_services::{BackupDataSource, BackupSection};
use std::{path::PathBuf, sync::Arc};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn gc_plan(resource: conduit_services::GcRunResource) -> conduit_services::GcRunPlan {
    conduit_services::GcRunPlan {
        steps: vec![conduit_services::GcRunStep {
            resource,
            cutoff_at: chrono::Utc::now() - chrono::Duration::days(1),
            retention_days: 1,
        }],
        run_vacuum: false,
        unknown_resources: Vec::new(),
    }
}

#[tokio::test]
async fn recovery_gc_tombstone_prevents_hydration_and_retries_external_deletion() -> TestResult {
    use conduit_db::repo::request_repo::RequestRepo;
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    let root = directory();
    let storage = conduit_storage::DataStorageService::new(
        conduit_storage::DataStorageKind::Local,
        Some(&conduit_storage::DataStorageConfig::from_value(
            &serde_json::json!({"directory":root.to_string_lossy()}),
        )?),
    )?;
    storage
        .save_data(
            "1/requests/1/response_body.json",
            b"{\"content\":\"retired\"}",
        )
        .await?;
    let object = root.join("objects/1/requests/1/response_body.json");
    let sid: i64 = sqlx::query_scalar("INSERT INTO data_storages(name,description,\"primary\",\"type\",settings,status) VALUES('gc','',FALSE,'fs',$1,'active') RETURNING id")
        .bind(sqlx::types::Json(serde_json::json!({"directory":root.to_string_lossy()}))).fetch_one(&db.pool).await?;
    sqlx::query("INSERT INTO requests(id,project_id,model_id,request_body,response_body,status,data_storage_id,created_at) VALUES(1,1,'gc','null',NULL,'completed',$1,now()-interval '2 days')").bind(sid).execute(&db.pool).await?;
    let config = conduit_services::GcConfig {
        cron: String::new(),
        vacuum_enabled: false,
        vacuum_full: false,
    };
    let report = crate::wiring_postgres_system_operations::execute_postgres_gc_plan(
        &db.pool,
        &config,
        &gc_plan(conduit_services::GcRunResource::ResponseBodies),
    )
    .await;
    assert!(report.steps.iter().all(|step| step.error.is_none()));
    let repo = Arc::new(conduit_db::PgDataStorageRepo::new(db.pool.clone()));
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    let mut row = conduit_db::PgRequestRepo::new(db.pool.clone())
        .find_request_by_id_unchecked(&ctx, "1")
        .await?
        .unwrap();
    conduit_db::PgRequestRepo::new(db.pool.clone())
        .update_request_unchecked(
            &ctx,
            "1",
            "1",
            conduit_db::repo::request_repo::UpdateRequestInput {
                response_body: Some(serde_json::json!({"late":"write"})),
                ..Default::default()
            },
        )
        .await?;
    crate::wiring_request_content::hydrate_request_artifacts(repo.as_ref(), &mut row).await;
    assert!(row.response_body.is_none());
    assert!(object.exists());
    let unavailable = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("DELETE"))
        .respond_with(wiremock::ResponseTemplate::new(500))
        .mount(&unavailable)
        .await;
    sqlx::query("UPDATE data_storages SET \"type\"='webdav',settings=$1 WHERE id=$2")
        .bind(sqlx::types::Json(
            serde_json::json!({"url":unavailable.uri()}),
        ))
        .bind(sid)
        .execute(&db.pool)
        .await?;
    crate::artifact_cleanup::retry(&db.pool, repo.clone()).await?;
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT attempts FROM artifact_deletion_queue")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    assert!(object.exists());
    sqlx::query("UPDATE data_storages SET \"type\"='fs',settings=$1 WHERE id=$2")
        .bind(sqlx::types::Json(
            serde_json::json!({"directory":root.to_string_lossy()}),
        ))
        .bind(sid)
        .execute(&db.pool)
        .await?;
    sqlx::query("UPDATE artifact_deletion_queue SET next_attempt_at=now()")
        .execute(&db.pool)
        .await?;
    crate::artifact_cleanup::retry(
        &db.pool,
        Arc::new(conduit_db::PgDataStorageRepo::new(db.pool.clone())),
    )
    .await?;
    assert!(!object.exists());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM artifact_deletion_queue")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    db.cleanup().await?;
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[tokio::test]
async fn recovery_live_policy_and_cancel_keep_request_and_execution_consistent() -> TestResult {
    use conduit_orchestrator::orchestrator::{
        OrchestratorContext, RequestRecorder, stream_final_plan,
    };
    use conduit_pipeline::{ExecutionMode, pipeline::AttemptRecord};
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    sqlx::query("INSERT INTO requests(id,project_id,model_id,request_body,status) VALUES(1,1,'live','{}','processing')").execute(&db.pool).await?;
    sqlx::query("INSERT INTO request_executions(id,project_id,request_id,model_id,request_body,status) VALUES(1,1,1,'live','{}','processing')").execute(&db.pool).await?;
    let recorder = crate::usage_log_recorder::UsageLogRecorder::new(Arc::new(
        conduit_db::InMemoryUsageRepo::new(),
    ))
    .with_postgres_stream_persistence(db.pool.clone());
    let mut ctx = OrchestratorContext::new();
    ctx.metadata
        .insert("storage_store_response_body".into(), "false".into());
    ctx.metadata
        .insert("storage_store_chunks".into(), "false".into());
    let response = conduit_llm::HttpResponse {
        json_body: Some(serde_json::json!({"content":"must not persist"})),
        ..Default::default()
    };
    let attempt = AttemptRecord {
        sequence: 1,
        channel_id: "1".into(),
        model_index: 0,
        mode: ExecutionMode::Stream,
        outcome: Ok(response.clone()),
    };
    recorder
        .record_success(&ctx, "1", "1", &attempt, &response)
        .await?;
    let chunks = vec![conduit_llm::StreamEvent {
        data: Some("must not persist".into()),
        ..Default::default()
    }];
    recorder
        .record_stream_request_chunks(&ctx, "1", "1", &chunks)
        .await?;
    recorder
        .record_stream_final(
            &ctx,
            "1",
            "1",
            &stream_final_plan(true, false),
            Some("1"),
            None,
            &chunks,
        )
        .await?;
    for table in ["requests", "request_executions"] {
        let stored: (
            String,
            Option<sqlx::types::Json<serde_json::Value>>,
            Option<sqlx::types::Json<serde_json::Value>>,
        ) = sqlx::query_as(&format!(
            "SELECT status,response_body,response_chunks FROM {table} WHERE id=1"
        ))
        .fetch_one(&db.pool)
        .await?;
        assert_eq!(stored.0, "completed");
        assert!(stored.1.is_none());
        assert!(stored.2.is_none());
        sqlx::query(&format!(
            "UPDATE {table} SET status='processing' WHERE id=1"
        ))
        .execute(&db.pool)
        .await?;
    }
    recorder
        .record_cancellation(
            &ctx,
            "1",
            "1",
            &conduit_core::ConduitError::internal("context canceled"),
        )
        .await?;
    recorder
        .record_stream_final(
            &ctx,
            "1",
            "1",
            &stream_final_plan(false, true),
            Some("1"),
            None,
            &[],
        )
        .await?;
    for table in ["requests", "request_executions"] {
        assert_eq!(
            sqlx::query_scalar::<_, String>(&format!("SELECT status FROM {table} WHERE id=1"))
                .fetch_one(&db.pool)
                .await?,
            "canceled"
        );
    }
    for message in ["provider canceled operation", "provider unavailable"] {
        for table in ["requests", "request_executions"] {
            sqlx::query(&format!(
                "UPDATE {table} SET status='processing' WHERE id=1"
            ))
            .execute(&db.pool)
            .await?;
        }
        recorder
            .record_failure(
                &ctx,
                "1",
                "1",
                &conduit_core::ConduitError::upstream(message),
            )
            .await?;
        recorder
            .record_stream_final(
                &ctx,
                "1",
                "1",
                &stream_final_plan(false, false),
                Some("1"),
                None,
                &[],
            )
            .await?;
        for table in ["requests", "request_executions"] {
            assert_eq!(
                sqlx::query_scalar::<_, String>(&format!("SELECT status FROM {table} WHERE id=1"))
                    .fetch_one(&db.pool)
                    .await?,
                "failed",
                "{table}: {message}"
            );
        }
    }
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn recovery_gc_retains_pending_metering_and_live_activity_until_expiry() -> TestResult {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    sqlx::query("INSERT INTO requests(id,project_id,model_id,request_body,status,created_at,updated_at,activity_until,metering_pending) VALUES(1,1,'live','{}','processing',now()-interval '2 days',now()-interval '2 days',now()+interval '10 minutes',FALSE),(2,1,'pending','{}','completed',now()-interval '2 days',now()-interval '2 days',NULL,TRUE)").execute(&db.pool).await?;
    sqlx::query("INSERT INTO request_executions(project_id,request_id,model_id,request_body,status,updated_at,activity_until) VALUES(1,1,'live','{}','processing',now()-interval '2 days',now()+interval '10 minutes')").execute(&db.pool).await?;
    assert_eq!(
        crate::maintenance::mark_stale_processing_postgres(
            &db.pool,
            std::time::Duration::from_secs(60)
        )
        .await?,
        0
    );
    let config = conduit_services::GcConfig {
        cron: String::new(),
        vacuum_enabled: false,
        vacuum_full: false,
    };
    crate::wiring_postgres_system_operations::execute_postgres_gc_plan(
        &db.pool,
        &config,
        &gc_plan(conduit_services::GcRunResource::Requests),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM requests")
            .fetch_one(&db.pool)
            .await?,
        2
    );
    sqlx::query("UPDATE requests SET activity_until=now()-interval '1 second' WHERE id=1")
        .execute(&db.pool)
        .await?;
    sqlx::query("UPDATE request_executions SET activity_until=now()-interval '1 second'")
        .execute(&db.pool)
        .await?;
    assert_eq!(
        crate::maintenance::mark_stale_processing_postgres(
            &db.pool,
            std::time::Duration::from_secs(60)
        )
        .await?,
        2
    );
    db.cleanup().await?;
    Ok(())
}

fn directory() -> PathBuf {
    std::env::var_os("CONDUIT_TEST_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("usage-regression-{}", uuid::Uuid::new_v4()))
}
fn config(path: &std::path::Path) -> UsageRecoveryConfig {
    UsageRecoveryConfig {
        directory: path.to_string_lossy().into(),
        ..Default::default()
    }
}
fn event(key: &str) -> MeteredEvent {
    MeteredEvent {
        version: 1,
        event_key: event_key(key),
        reservation_key: None,
        usage: CreateUsageLogInput {
            id: String::new(),
            project_id: "1".into(),
            request_id: "1".into(),
            api_key_id: None,
            channel_id: None,
            model_id: "metered".into(),
            prompt_tokens: 3,
            completion_tokens: 4,
            total_tokens: 7,
            prompt_audio_tokens: 0,
            prompt_cached_tokens: 0,
            prompt_write_cached_tokens: 0,
            prompt_write_cached_tokens_5m: 0,
            prompt_write_cached_tokens_1h: 0,
            completion_audio_tokens: 0,
            completion_reasoning_tokens: 0,
            completion_accepted_prediction_tokens: 0,
            completion_rejected_prediction_tokens: 0,
            source: "api".into(),
            format: "openai/chat_completions".into(),
            total_cost: None,
            cost_items: serde_json::json!([]),
            cost_price_reference_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    }
}

#[test]
fn recovery_journal_is_bounded_exclusive_and_recovers_missing_index() -> TestResult {
    let path = directory();
    let mut cfg = config(&path);
    cfg.max_event_bytes = 2048;
    cfg.max_bytes = 8192;
    let journal = UsageJournal::open(cfg.clone())?;
    assert!(UsageJournal::open(cfg.clone()).is_err());
    journal.reserve("first")?;
    assert!(journal.reserve("second").is_err());
    let event = event("first");
    journal.put_ready(&event)?;
    let ready = path.join(format!("{}.ready", event.event_key));
    std::fs::remove_file(&ready)?; // Crash after fsynced input but before index creation.
    drop(journal);
    let restarted = UsageJournal::open(cfg.clone())?;
    assert!(ready.exists());
    assert_eq!(
        serde_json::from_slice::<MeteredEvent>(&std::fs::read(&ready)?)?
            .usage
            .total_tokens,
        7
    );
    restarted.ack(&event.event_key)?;
    drop(restarted);
    std::fs::write(path.join("journal.wal"), b"truncated input")?;
    let corrupt = UsageJournal::open(cfg)?;
    assert!(corrupt.reserve("new").is_err());
    drop(corrupt);
    std::fs::remove_dir_all(path)?;
    Ok(())
}

#[tokio::test]
async fn recovery_pg_failure_restart_duplicate_and_hash_conflict() -> TestResult {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    sqlx::query("INSERT INTO requests(id,project_id,model_id,request_body,status) VALUES(1,1,'metered','{}','completed')").execute(&db.pool).await?;
    sqlx::query("CREATE FUNCTION fail_metering() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected first usage failure'; END $$").execute(&db.pool).await?;
    sqlx::query("CREATE TRIGGER fail_metering BEFORE INSERT ON usage_logs FOR EACH ROW EXECUTE FUNCTION fail_metering()").execute(&db.pool).await?;
    let path = directory();
    let cfg = config(&path);
    let journal = UsageJournal::open(cfg.clone())?;
    journal.reserve("recovery")?;
    let event = event("recovery");
    journal.put_ready(&event)?;
    assert!(handoff(&db.pool, &event).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_recovery_receipts")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    assert!(path.join(format!("{}.ready", event.event_key)).exists());
    drop(journal);
    sqlx::query("DROP TRIGGER fail_metering ON usage_logs")
        .execute(&db.pool)
        .await?;
    let journal = UsageJournal::open(cfg)?;
    assert_eq!(journal.replay(&db.pool).await?, 1);
    let (a, b) = tokio::join!(handoff(&db.pool, &event), handoff(&db.pool, &event));
    assert_eq!(a?.unwrap().id, b?.unwrap().id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_logs")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_charge_outbox")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    sqlx::query("UPDATE usage_charge_outbox SET status='completed'")
        .execute(&db.pool)
        .await?;
    handoff(&db.pool, &event).await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM usage_charge_outbox")
            .fetch_one(&db.pool)
            .await?,
        "completed"
    );
    let mut changed = event.clone();
    changed.usage.total_tokens += 1;
    assert!(
        handoff(&db.pool, &changed)
            .await
            .unwrap_err()
            .contains("payload conflict")
    );
    sqlx::query("DELETE FROM usage_logs")
        .execute(&db.pool)
        .await?;
    assert!(handoff(&db.pool, &event).await?.is_none());
    sqlx::query("DELETE FROM usage_charge_outbox")
        .execute(&db.pool)
        .await?;
    assert!(
        handoff(&db.pool, &event)
            .await
            .unwrap_err()
            .contains("manual reconciliation")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_logs")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    drop(journal);
    std::fs::remove_dir_all(path)?;
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn recovery_backup_sections_share_snapshot_during_concurrent_change() -> TestResult {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    sqlx::query("INSERT INTO projects(id,name) VALUES(999,'before')")
        .execute(&db.pool)
        .await?;
    sqlx::query("INSERT INTO api_keys(id,project_id,name,key,scopes) VALUES(999,999,'snapshot','fake-test-key','[]')").execute(&db.pool).await?;
    let mut writer = db.pool.begin().await?;
    sqlx::query("LOCK TABLE api_keys IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await?;
    let source = crate::wiring_postgres_backup::PgBackupDataSourceAdapter::new(db.pool.clone());
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    let read = tokio::spawn(async move {
        source
            .load_sections(&ctx, &[BackupSection::Projects, BackupSection::ApiKeys])
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE wait_event_type='Lock' AND query LIKE 'SELECT to_jsonb(api_key)%')").fetch_one(&db.pool).await.unwrap();
            if blocked { break; } tokio::task::yield_now().await;
        }
    }).await?;
    sqlx::query("UPDATE projects SET name='after' WHERE id=999")
        .execute(&mut *writer)
        .await?;
    writer.commit().await?;
    let sections = read.await??;
    let project = sections[&BackupSection::Projects]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == 999)
        .unwrap();
    let key = sections[&BackupSection::ApiKeys]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == 999)
        .unwrap();
    assert_eq!(project["name"], "before");
    assert_eq!(key["project_name"], "before");
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn recovery_settings_patch_preserves_concurrent_config_and_worker_state() -> TestResult {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    let repo = Arc::new(conduit_db::PgSystemRepo::new(db.pool.clone()));
    let system = conduit_services::SystemService::from_system_repo(
        repo,
        Arc::new(conduit_cache::NoopCache::new()),
    );
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    let (a, b) = tokio::join!(
        system.patch_system_value(
            &ctx,
            "concurrent",
            serde_json::json!({"enabled":false,"target":2})
        ),
        system.patch_system_value(
            &ctx,
            "concurrent",
            serde_json::json!({"last_backup_at":"finished","last_backup_error":null})
        )
    );
    a?;
    b?;
    let value = system.get_system_value(&ctx, "concurrent").await?.unwrap();
    assert_eq!(value["enabled"], false);
    assert_eq!(value["target"], 2);
    assert_eq!(value["last_backup_at"], "finished");
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn recovery_claim_excludes_other_instances_and_recovers_expired_owner() -> TestResult {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    let pool = db.pool.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let first = tokio::spawn(async move {
        crate::maintenance_claim::run(&pool, "same", false, async {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
            Ok(())
        })
        .await
    });
    entered_rx.await?;
    assert!(
        crate::maintenance_claim::run(&db.pool, "same", false, async {
            panic!("duplicate side effect");
            #[allow(unreachable_code)]
            Ok(())
        })
        .await?
        .is_none()
    );
    release_tx.send(()).unwrap();
    assert!(first.await??.is_some());
    sqlx::query("INSERT INTO maintenance_claims(job_key,owner,lease_until) VALUES('expired',$1,now()-interval '1 second')").bind(uuid::Uuid::new_v4()).execute(&db.pool).await?;
    assert!(
        crate::maintenance_claim::run(&db.pool, "expired", true, async { Ok(()) })
            .await?
            .is_some()
    );
    assert!(
        crate::maintenance_claim::run(&db.pool, "expired", true, async { Ok(()) })
            .await?
            .is_none()
    );
    db.cleanup().await?;
    Ok(())
}

use super::*;
use conduit_admin_graphql::model_ext::{
    ModelPriceInput, ModelPriceItemInput, PricingInput, PricingMode, SaveChannelModelPriceInput,
};
use conduit_admin_graphql::{request_usage::PriceItemCode, scalars::DecimalScalar};
use package::{ChangePackageServices, ChangeSetAvailableAction as Action, PackageViewer};
use rust_decimal::Decimal;

fn input(key: &str, value: Decimal) -> SaveChannelModelPriceInput {
    SaveChannelModelPriceInput {
        model_id: key.into(),
        currency_code: "CNY".into(),
        billing_expression: None,
        price: Some(ModelPriceInput {
            items: [
                PriceItemCode::PromptTokens,
                PriceItemCode::CompletionTokens,
                PriceItemCode::PromptCachedTokens,
                PriceItemCode::PromptWriteCachedTokens,
            ]
            .into_iter()
            .map(|item_code| ModelPriceItemInput {
                item_code,
                pricing: PricingInput {
                    mode: PricingMode::UsagePerUnit,
                    flat_fee: None,
                    usage_per_unit: Some(DecimalScalar(value)),
                    usage_tiered: None,
                },
                prompt_write_cache_variants: None,
            })
            .collect(),
        }),
    }
}
async fn db()
-> Result<Option<crate::postgres_test_support::IsolatedPostgres>, Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(None);
    };
    let db = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
    for id in [7_i64, 8] {
        sqlx::query("INSERT INTO users(id,email,password,status,is_owner) VALUES($1,$2,'test-hash','activated',true)").bind(id).bind(format!("package-owner-{id}@contract.test")).execute(&db.pool).await?;
    }
    Ok(Some(db))
}
async fn channel(pool: &PgPool) -> Result<ID, sqlx::Error> {
    let id=sqlx::query_scalar::<_,i64>("INSERT INTO channels(\"type\",name,status,credentials,supported_models,default_test_model,settings) VALUES('openai','r3-newapi','enabled','{}','[]','', '{\"managementAdapter\":\"new_api\",\"billingCurrency\":\"CNY\",\"rechargeMultiplier\":\"1\"}') RETURNING id").fetch_one(pool).await?;
    Ok(ID(id.to_string()))
}
async fn create(
    adapter: &PgChangeSetAdapter,
    channel: &ID,
    op: Uuid,
    prices: Vec<SaveChannelModelPriceInput>,
) -> Result<package::CreateChangeSetResult, gql::ChangeSetError> {
    let base = adapter.provider_edit_base(channel.clone()).await?;
    adapter
        .create_provider(
            7,
            channel.clone(),
            ID(op.to_string()),
            base.revision,
            prices,
            None,
        )
        .await
}
async fn fresh(adapter: &PgChangeSetAdapter, id: &ID) -> package::ChangeSetDetail {
    adapter
        .get_detail(
            PackageViewer {
                actor_user_id: 7,
                can_write: true,
            },
            id.clone(),
        )
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn provider_create_and_replay_report_committed_detail_failure_as_storage()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let op = Uuid::new_v4();
    sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES($1,$2,now(),now())")
        .bind(crate::wiring_governance::KEY)
        .bind(r#"{"revision":"invalid","enabled":false}"#)
        .execute(&db.pool)
        .await?;

    for _ in 0..2 {
        let error = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)])
            .await
            .unwrap_err();
        assert!(
            matches!(&error, gql::ChangeSetError::Storage(message) if message == "invalid governance settings")
        );
        let graphql_error = package::graphql_error(error);
        assert_eq!(
            graphql_error
                .extensions
                .as_ref()
                .and_then(|ext| ext.get("code")),
            Some(&async_graphql::Value::String("STORAGE_FAILED".into()))
        );
    }
    let committed_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT c.id FROM change_sets c JOIN channel_operation_receipts r \
         ON r.operation_id = c.operation_id \
         AND (r.stage_results #>> '{providerPriceEdit,changeSetID}')::bigint = c.id \
         WHERE c.operation_id = $1",
    )
    .bind(op)
    .fetch_all(&db.pool)
    .await?;
    assert_eq!(committed_ids.len(), 1);
    sqlx::query("DELETE FROM systems WHERE key=$1")
        .bind(crate::wiring_governance::KEY)
        .execute(&db.pool)
        .await?;
    sqlx::query("ALTER TABLE change_set_items RENAME TO unavailable_change_set_items")
        .execute(&db.pool)
        .await?;
    let error = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)])
        .await
        .unwrap_err();
    assert!(matches!(error, gql::ChangeSetError::Storage(_)));
    sqlx::query("ALTER TABLE unavailable_change_set_items RENAME TO change_set_items")
        .execute(&db.pool)
        .await?;
    let recovered = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)])
        .await?
        .change_set_detail
        .unwrap();
    assert_eq!(recovered.summary.id.as_str(), committed_ids[0].to_string());
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_manual_intents_are_distinct_idempotent_and_revision_fenced()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let op = Uuid::new_v4();
    let first = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)])
        .await?
        .change_set_detail
        .unwrap();
    let retried = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)])
        .await?
        .change_set_detail
        .unwrap();
    assert_eq!(first.summary.id, retried.summary.id);
    assert!(
        create(&adapter, &channel, op, vec![input("m", Decimal::TWO)])
            .await
            .unwrap_err()
            .to_string()
            .contains("OPERATION_CONFLICT")
    );
    let second = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("m", Decimal::ONE)],
    )
    .await?
    .change_set_detail
    .unwrap();
    assert_ne!(first.summary.id, second.summary.id);
    let saved = adapter
        .save_provider(
            7,
            first.summary.id.clone(),
            first.summary.revision.clone(),
            vec![input("m", Decimal::TWO)],
        )
        .await?;
    assert_ne!(saved.summary.revision, first.summary.revision);
    assert!(
        adapter
            .save_provider(
                7,
                first.summary.id.clone(),
                first.summary.revision,
                vec![input("m", Decimal::ONE)]
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("REVISION_CONFLICT")
    );
    assert_eq!(
        fresh(&adapter, &second.summary.id).await.summary.revision,
        second.summary.revision
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_model_prices")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_all_items_validate_and_invalid_apply_writes_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let detail = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("valid", Decimal::ONE), input("bad", -Decimal::ONE)],
    )
    .await?
    .change_set_detail
    .unwrap();
    assert!(
        adapter
            .transition(
                7,
                detail.summary.id.clone(),
                detail.summary.revision,
                Action::Submit,
                None
            )
            .await
            .is_err()
    );
    let persisted = fresh(&adapter, &detail.summary.id).await;
    assert_eq!(persisted.summary.status, gql::ChangeSetStatus::Draft);
    assert_eq!(persisted.summary.item_count, 2);
    assert_eq!(persisted.summary.invalid_item_count, 1);
    sqlx::query("UPDATE change_sets SET status='pending_review' WHERE id=$1")
        .bind(parse_numeric_id(detail.summary.id.as_str(), "id")?)
        .execute(&db.pool)
        .await?;
    let current = fresh(&adapter, &detail.summary.id).await;
    assert!(
        adapter
            .transition(
                8,
                current.summary.id.clone(),
                current.summary.revision,
                Action::ApproveApply,
                None
            )
            .await
            .is_err()
    );
    assert_eq!(
        fresh(&adapter, &detail.summary.id).await.summary.status,
        gql::ChangeSetStatus::Invalid
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_model_prices")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pricing_change_audits")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_concurrent_approval_applies_once_and_nochange_has_durable_receipt()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let detail = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("m", Decimal::ONE)],
    )
    .await?
    .change_set_detail
    .unwrap();
    let submitted = adapter
        .transition(
            7,
            detail.summary.id,
            detail.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    let (a, b) = tokio::join!(
        adapter.transition(
            8,
            submitted.summary.id.clone(),
            submitted.summary.revision.clone(),
            Action::ApproveApply,
            None
        ),
        adapter.transition(
            8,
            submitted.summary.id.clone(),
            submitted.summary.revision.clone(),
            Action::ApproveApply,
            None
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let applied = fresh(&adapter, &submitted.summary.id).await;
    assert_eq!(applied.summary.status, gql::ChangeSetStatus::Applied);
    assert_eq!(applied.applied_targets.len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_model_prices WHERE deleted_at=0"
        )
        .fetch_one(&db.pool)
        .await?,
        1
    );
    let op = Uuid::new_v4();
    let nochange = create(&adapter, &channel, op, vec![input("m", Decimal::ONE)]).await?;
    assert_eq!(nochange.outcome, package::CreateChangeSetOutcome::NoChange);
    assert!(nochange.change_set_detail.is_none());
    assert!(
        adapter
            .find_provider_operation(
                PackageViewer {
                    actor_user_id: 7,
                    can_write: true
                },
                ID(op.to_string())
            )
            .await?
            .is_some()
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_source_change_is_persisted_superseded_without_partial_price_write()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let d = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("m", Decimal::ONE), input("n", Decimal::ONE)],
    )
    .await?
    .change_set_detail
    .unwrap();
    let d = adapter
        .transition(7, d.summary.id, d.summary.revision, Action::Submit, None)
        .await?;
    sqlx::query("INSERT INTO channel_model_prices(channel_id,model_id,currency_code,price,reference_id) VALUES($1,'n','CNY','{\"items\":[]}','external-price')").bind(parse_numeric_id(channel.as_str(),"channel")?).execute(&db.pool).await?;
    let error = adapter
        .transition(
            8,
            d.summary.id.clone(),
            d.summary.revision,
            Action::ApproveApply,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SOURCE_CHANGED"));
    assert_eq!(
        fresh(&adapter, &d.summary.id).await.summary.status,
        gql::ChangeSetStatus::Superseded
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_model_prices")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_summary_pages_count_all_parents_and_bind_cursors()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    sqlx::query("INSERT INTO change_sets(kind,scope_type,scope_id,title,status,created_at,updated_at) SELECT 'retail_price','price_book','1','package-'||n,'pending_review',now(),now() FROM generate_series(1,27) n").execute(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let filter = package::ChangeSetFilterInput {
        kind: None,
        allowed_kinds: None,
        actor_user_id: None,
        statuses: None,
        scope_type: None,
        scope_id: None,
        search: None,
    };
    let page = adapter.list_summaries(filter.clone(), None, 20).await?;
    assert_eq!(page.edges.len(), 20);
    assert_eq!(page.total_count, 27);
    assert!(page.page_info.has_next_page);
    let next = adapter
        .list_summaries(filter, page.page_info.end_cursor.clone(), 20)
        .await?;
    assert_eq!(next.edges.len(), 7);
    assert!(!next.page_info.has_next_page);
    assert!(
        !next
            .edges
            .iter()
            .any(|n| page.edges.iter().any(|p| p.node.id == n.node.id))
    );
    let changed = package::ChangeSetFilterInput {
        kind: None,
        allowed_kinds: None,
        actor_user_id: None,
        statuses: None,
        scope_type: None,
        scope_id: None,
        search: Some("other".into()),
    };
    assert!(
        adapter
            .list_summaries(changed, page.page_info.end_cursor, 20)
            .await
            .unwrap_err()
            .to_string()
            .contains("BAD_CURSOR")
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_mapping_fan_in_validates_shared_before_then_creates_one_model()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let id = parse_numeric_id(channel.as_str(), "channel")?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let mut tx = db.pool.begin().await?;
    let mut proposals = Vec::new();
    for model in ["upstream-a", "upstream-b"] {
        let deployment=sqlx::query_scalar::<_,i64>("INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,status) VALUES($1,$2,$2,'enabled') RETURNING id").bind(id).bind(model).fetch_one(&mut *tx).await?;
        proposals.push(Proposal{key:format!("{model}:shared"),action:"create".into(),before:None,after:Some(json!({"deploymentID":deployment,"publicModel":{"modelID":"shared","developer":"test","type":"chat","name":"Shared","group":"chat"},"routeStatus":"enabled"})),source:json!({"channelID":id}),error:None});
    }
    let staged = stage_observed(
        &mut tx,
        &OperationContext::automatic(),
        "model_mapping",
        id,
        "fan in",
        "1",
        &proposals,
        Utc::now(),
    )
    .await?
    .unwrap();
    tx.commit().await?;
    let d = fresh(&adapter, &ID(staged.id.to_string())).await;
    let applied = adapter
        .transition(
            8,
            d.summary.id,
            d.summary.revision,
            Action::ApproveApply,
            None,
        )
        .await?;
    assert_eq!(applied.applied_targets.len(), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM models WHERE model_id='shared'")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM model_routes")
            .fetch_one(&db.pool)
            .await?,
        2
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_expired_attempt_fence_never_commits_and_read_marks_interrupted()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let op = Uuid::new_v4();
    let id = parse_numeric_id(channel.as_str(), "channel")?;
    sqlx::query("INSERT INTO channel_operation_receipts(operation_id,channel_id,operation_type,origin,request_fingerprint,attempt,state,lease_expires_at,stage_results,created_at,updated_at) VALUES($1,$2,'NEW_API_DISCOVERY','USER_SYNC','intent',1,'RUNNING',now()-interval '1 second','{}',now(),now())").bind(op).bind(id).execute(&db.pool).await?;
    let mut tx = db.pool.begin().await?;
    let fence = crate::wiring_postgres_channel_discovery::DiscoveryFence {
        operation_id: op,
        attempt: 1,
    };
    assert!(
        crate::wiring_postgres_channel_discovery::lock_fence(&mut tx, &fence)
            .await
            .is_err()
    );
    tx.rollback().await?;
    sqlx::query("UPDATE channel_operation_receipts SET attempt=2,lease_expires_at=now()+interval '1 minute' WHERE operation_id=$1").bind(op).execute(&db.pool).await?;
    let mut tx = db.pool.begin().await?;
    assert!(
        crate::wiring_postgres_channel_discovery::lock_fence(&mut tx, &fence)
            .await
            .is_err()
    );
    tx.rollback().await?;
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_missing_cache_price_blocks_the_entire_manual_package()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let mut incomplete = input("incomplete", Decimal::ONE);
    incomplete.price.as_mut().unwrap().items.pop();
    let detail = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("valid", Decimal::ONE), incomplete],
    )
    .await?
    .change_set_detail
    .unwrap();
    assert_eq!(detail.summary.invalid_item_count, 1);
    let error = adapter
        .transition(
            7,
            detail.summary.id.clone(),
            detail.summary.revision,
            Action::Submit,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("VALIDATION_FAILED"));
    assert!(
        fresh(&adapter, &detail.summary.id)
            .await
            .issues
            .iter()
            .any(|issue| issue.message.contains("prompt_write_cached_tokens"))
    );
    assert_eq!(
        fresh(&adapter, &detail.summary.id).await.summary.status,
        gql::ChangeSetStatus::Draft
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_model_prices")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_automatic_dedupe_and_supersession_use_the_complete_observation()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let channel = channel(&db.pool).await?;
    let channel_id = parse_numeric_id(channel.as_str(), "channel")?;
    let proposals = |snapshot: i64| {
        ["a","b"].into_iter().enumerate().map(|(i,key)|Proposal{key:key.into(),action:"create".into(),before:None,after:Some(json!({"items":[]})),source:json!({"source":"observed","snapshotID":snapshot,"providerPriceRowID":snapshot*10+i as i64,"unit":"CHANNEL_BALANCE_UNIT"}),error:None}).collect::<Vec<_>>()
    };
    let mut tx = db.pool.begin().await?;
    let original = stage_observed(
        &mut tx,
        &OperationContext::automatic(),
        "provider_price",
        channel_id,
        "full observation",
        "100",
        &proposals(100),
        Utc::now(),
    )
    .await?
    .unwrap();
    tx.commit().await?;
    let mut tx = db.pool.begin().await?;
    let refreshed = stage_observed(
        &mut tx,
        &OperationContext::automatic(),
        "provider_price",
        channel_id,
        "full observation",
        "101",
        &proposals(101),
        Utc::now(),
    )
    .await?
    .unwrap();
    assert!(refreshed.reused);
    assert_eq!(original.id, refreshed.id);
    assert_eq!(refreshed.item_count, 2);
    tx.commit().await?;
    let refs=sqlx::query_scalar::<_,String>("SELECT source_snapshot->>'snapshotID' FROM change_set_items WHERE change_set_id=$1 ORDER BY item_key").bind(original.id).fetch_all(&db.pool).await?;
    assert_eq!(refs, vec!["101", "101"]);
    let human = OperationContext {
        operation_id: Uuid::new_v4(),
        origin: package::OperationOrigin::UserSync,
        actor_user_id: Some(7),
    };
    let mut tx = db.pool.begin().await?;
    let human = stage_observed(
        &mut tx,
        &human,
        "provider_price",
        channel_id,
        "user observation",
        "102",
        &proposals(102),
        Utc::now(),
    )
    .await?
    .unwrap();
    tx.commit().await?;
    assert_ne!(human.id, original.id);
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let manual = create(
        &adapter,
        &channel,
        Uuid::new_v4(),
        vec![input("manual", Decimal::ONE)],
    )
    .await?
    .change_set_detail
    .unwrap();
    let mut tx = db.pool.begin().await?;
    let replacement = stage_observed(
        &mut tx,
        &OperationContext::automatic(),
        "provider_price",
        channel_id,
        "changed observation",
        "103",
        &proposals(103)[..1],
        Utc::now(),
    )
    .await?
    .unwrap();
    tx.commit().await?;
    assert_ne!(replacement.id, original.id);
    assert_eq!(
        fresh(&adapter, &ID(original.id.to_string()))
            .await
            .summary
            .status,
        gql::ChangeSetStatus::Superseded
    );
    assert_eq!(
        fresh(&adapter, &ID(human.id.to_string()))
            .await
            .summary
            .status,
        gql::ChangeSetStatus::PendingReview
    );
    assert_eq!(
        fresh(&adapter, &manual.summary.id).await.summary.status,
        gql::ChangeSetStatus::Draft
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_retail_publication_keeps_untouched_prices_and_removes_unchanged_delta()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = db().await? else { return Ok(()) };
    let adapter = PgChangeSetAdapter::new(db.pool.clone());
    let mut models = Vec::new();
    for name in ["retail-a", "retail-b"] {
        models.push(sqlx::query_scalar::<_,i64>("INSERT INTO models(model_id,name,developer,\"type\",\"group\",icon,model_card,settings,status) VALUES($1,$1,'test','chat','chat','test','{}'::jsonb,'{}'::jsonb,'enabled') RETURNING id").bind(name).fetch_one(&db.pool).await?);
    }
    let book=sqlx::query_scalar::<_,i64>("INSERT INTO price_books(name,currency,is_default,created_at,updated_at) VALUES('r3-retail','CNY',true,now(),now()) RETURNING id").fetch_one(&db.pool).await?;
    let version=sqlx::query_scalar::<_,i64>("INSERT INTO price_book_versions(price_book_id,version,status,reference_id,effective_start_at,created_at,updated_at) VALUES($1,1,'published','r3-base',now(),now(),now()) RETURNING id").bind(book).fetch_one(&db.pool).await?;
    let original = serde_json::to_value(crate::conv::model_price_input_to_core(
        input("unused", Decimal::ONE).price.unwrap(),
    ))?;
    for model in &models {
        sqlx::query("INSERT INTO price_book_items(price_book_version_id,public_model_id,price,created_at,updated_at) VALUES($1,$2,$3,now(),now())").bind(version).bind(model).bind(SqlJson(original.clone())).execute(&db.pool).await?;
    }
    let book = ID(book.to_string());
    let base = adapter.retail_edit_base(book.clone()).await?;
    let op = ID(Uuid::new_v4().to_string());
    let d = adapter
        .create_retail(7, book.clone(), op.clone(), base.revision.clone())
        .await?;
    let recovered = adapter
        .find_by_operation(
            PackageViewer {
                actor_user_id: 7,
                can_write: true,
            },
            op,
            gql::ChangeSetKind::RetailPrice,
            "price_book".into(),
            ID(format!("gid://conduit/PriceBook/{}", book.as_str())),
        )
        .await?
        .unwrap();
    assert_eq!(recovered.summary.id, d.summary.id);
    let unchanged = adapter
        .save_retail_item(
            7,
            gql::SaveRetailPriceChangeSetItemInput {
                change_set_id: d.summary.id.clone(),
                expected_revision: d.summary.revision,
                public_model_id: ID(models[0].to_string()),
                price: async_graphql::Json(original.clone()),
            },
        )
        .await?;
    assert_eq!(fresh(&adapter, &d.summary.id).await.summary.item_count, 0);
    let changed = serde_json::to_value(crate::conv::model_price_input_to_core(
        input("unused", Decimal::TWO).price.unwrap(),
    ))?;
    adapter
        .save_retail_item(
            7,
            gql::SaveRetailPriceChangeSetItemInput {
                change_set_id: d.summary.id.clone(),
                expected_revision: unchanged.change_set_revision,
                public_model_id: ID(models[0].to_string()),
                price: async_graphql::Json(changed.clone()),
            },
        )
        .await?;
    let d = fresh(&adapter, &d.summary.id).await;
    let d = adapter
        .transition(7, d.summary.id, d.summary.revision, Action::Submit, None)
        .await?;
    adapter
        .transition(
            8,
            d.summary.id,
            d.summary.revision,
            Action::ApproveApply,
            None,
        )
        .await?;
    let rows=sqlx::query_as::<_,(i64,SqlJson<Value>)>("SELECT i.public_model_id,i.price FROM price_book_items i JOIN price_book_versions v ON v.id=i.price_book_version_id WHERE v.price_book_id=$1 AND v.status='published' ORDER BY i.public_model_id").bind(parse_numeric_id(book.as_str(),"book")?).fetch_all(&db.pool).await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1.0, changed);
    assert_eq!(rows[1].1.0, original);
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn r3_new_api_create_atomically_binds_channel_receipt_and_normalized_intent()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_admin_graphql::channel::{
        ChannelCredentialsInput, ChannelMutationServices, ChannelPublicModelBindingInput,
        ChannelPublicModelBindingsInput, ChannelSettingsInput, ChannelType, CreateChannelInput,
        UpdateChannelInput,
    };
    let Some(db) = db().await? else { return Ok(()) };
    let repo = std::sync::Arc::new(conduit_db::PgChannelRepo::new(db.pool.clone()));
    let adapter =
        crate::wiring_channel_crud::ChannelCrudAdapter::new(repo).with_pool(db.pool.clone());
    let make = |op: Uuid, name: &str| CreateChannelInput {
        public_model_bindings: Some(ChannelPublicModelBindingsInput {
            upserts: Some(vec![ChannelPublicModelBindingInput {
                upstream_model_id: "provider-model".into(),
                public_model_key: "public-model".into(),
                model_type: Some(conduit_admin_graphql::model::ModelType::Embedding),
                confirm_compatibility: None,
            }]),
            remove_route_ids: None,
        }),
        operation_id: Some(ID(op.to_string())),
        channel_type: ChannelType::Openai,
        base_url: Some("http://127.0.0.1:32123/prefix/v1/?ignored=yes#fragment".into()),
        website_url: None,
        quota_currency: Some("USD".into()),
        actual_quota_used: None,
        quota_remaining: None,
        name: name.into(),
        credentials: ChannelCredentialsInput {
            api_keys: Some(vec!["test-key-not-a-live-provider".into()]),
            new_api_pat: Some("test-pat".into()),
            new_api_user_id: Some("12".into()),
            ..Default::default()
        },
        supported_models: vec!["provider-model".into()],
        manual_models: None,
        auto_sync_supported_models: Some(false),
        auto_sync_model_pattern: None,
        tags: None,
        default_test_model: String::new(),
        policies: None,
        settings: Some(ChannelSettingsInput {
            management_adapter: Some("new_api".into()),
            ..Default::default()
        }),
        ordering_weight: None,
        remark: None,
        endpoints: None,
    };
    let op = Uuid::new_v4();
    let input = make(op, "new-api-atomic");
    let blocked = adapter
        .create_channel(Some(7), input.clone())
        .await
        .unwrap_err();
    assert!(
        blocked
            .to_string()
            .contains("APPROVAL_REQUIRED: model_mapping")
    );
    let unwritten = sqlx::query_scalar::<_, i64>(
        "SELECT (SELECT COUNT(*) FROM channels)+(SELECT COUNT(*) FROM models)+(SELECT COUNT(*) FROM channel_operation_receipts)"
    ).fetch_one(&db.pool).await?;
    assert_eq!(unwritten, 0);
    use conduit_admin_graphql::governance::{GovernanceServices, UpdateGovernanceSettingsInput};
    crate::wiring_governance::PgGovernanceAdapter::new(db.pool.clone())
        .update_settings(
            7,
            UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;

    let first = adapter.create_channel(Some(7), input.clone()).await?;
    let again = adapter.create_channel(Some(7), input.clone()).await?;
    assert_eq!(first.id, again.id);
    assert_eq!(
        first.base_url.as_deref(),
        Some("http://127.0.0.1:32123/prefix")
    );
    assert_eq!(first.website_url, first.base_url);
    assert!(
        adapter
            .create_channel(Some(8), input.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("OPERATION_CONFLICT")
    );
    let mut changed = input;
    changed.name = "different-intent".into();
    assert!(
        adapter
            .create_channel(Some(7), changed)
            .await
            .unwrap_err()
            .to_string()
            .contains("OPERATION_CONFLICT")
    );
    let id = parse_numeric_id(first.id.as_str(), "channel")?;
    let (state,actor)=sqlx::query_as::<_,(String,i64)>("SELECT state,created_by FROM channel_operation_receipts WHERE operation_id=$1 AND channel_id=$2").bind(op).bind(id).fetch_one(&db.pool).await?;
    assert_eq!(state, "NOT_STARTED");
    assert_eq!(actor, 7);
    let (route_id, model_type, model_status, route_status) = sqlx::query_as::<_, (i64, String, String, String)>(
        r#"SELECT r.id,m."type",m.status,r.status FROM model_routes r
           JOIN models m ON m.id=r.public_model_id
           JOIN upstream_model_deployments d ON d.id=r.deployment_id
           WHERE m.model_id='public-model' AND d.channel_id=$1 AND d.upstream_model_id='provider-model'"#
    ).bind(id).fetch_one(&db.pool).await?;
    assert_eq!(model_type, "embedding");
    assert_eq!(model_status, "disabled");
    assert_eq!(route_status, "disabled");
    let err = adapter
        .update_channel(
            Some(7),
            first.id.as_str(),
            UpdateChannelInput {
                name: Some("must-not-rename".into()),
                public_model_bindings: Some(ChannelPublicModelBindingsInput {
                    upserts: Some(vec![ChannelPublicModelBindingInput {
                        upstream_model_id: "provider-model".into(),
                        public_model_key: "public-model".into(),
                        model_type: None,
                        confirm_compatibility: None,
                    }]),
                    remove_route_ids: Some(vec![ID(route_id.to_string())]),
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("both upserted and removed"));
    let stored_name = sqlx::query_scalar::<_, String>("SELECT name FROM channels WHERE id=$1")
        .bind(id)
        .fetch_one(&db.pool)
        .await?;
    assert_eq!(stored_name, "new-api-atomic");

    let updated = adapter
        .update_channel(
            None,
            first.id.as_str(),
            UpdateChannelInput {
                base_url: Some("http://127.0.0.1:32123/changed/v1?x=1".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        updated.base_url.as_deref(),
        Some("http://127.0.0.1:32123/changed")
    );
    assert_eq!(updated.website_url, updated.base_url);
    let distinct = adapter
        .update_channel(
            Some(7),
            first.id.as_str(),
            UpdateChannelInput {
                website_url: Some("http://127.0.0.1:32124/console".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(distinct.base_url, updated.base_url);
    let preserved = adapter
        .update_channel(
            Some(7),
            first.id.as_str(),
            UpdateChannelInput {
                base_url: Some("http://127.0.0.1:32123/new-base/v1".into()),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(preserved.website_url, distinct.website_url);
    let cleared = adapter
        .update_channel(
            Some(7),
            first.id.as_str(),
            UpdateChannelInput {
                clear_website_url: Some(true),
                ..Default::default()
            },
        )
        .await?;
    assert!(cleared.website_url.is_none());
    let unchanged = adapter
        .update_channel(
            Some(7),
            first.id.as_str(),
            UpdateChannelInput {
                remark: Some("updated remark".into()),
                ..Default::default()
            },
        )
        .await?;
    assert!(unchanged.website_url.is_none());

    sqlx::query("ALTER TABLE channel_operation_receipts ADD CONSTRAINT r3_injected_receipt_failure CHECK (state <> 'NOT_STARTED') NOT VALID").execute(&db.pool).await?;
    let mut failure = make(Uuid::new_v4(), "must-rollback");
    failure
        .public_model_bindings
        .as_mut()
        .unwrap()
        .upserts
        .as_mut()
        .unwrap()[0]
        .public_model_key = "rollback-model".into();
    assert!(adapter.create_channel(Some(7), failure).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE name='must-rollback'")
            .fetch_one(&db.pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channel_operation_receipts")
            .fetch_one(&db.pool)
            .await?,
        1
    );
    let models = sqlx::query_scalar::<_, String>("SELECT model_id FROM models ORDER BY id")
        .fetch_all(&db.pool)
        .await?;
    assert_eq!(models, ["public-model"]);
    db.cleanup().await?;
    Ok(())
}

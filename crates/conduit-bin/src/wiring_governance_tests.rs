#![cfg(test)]

use super::*;
use async_graphql::{ID, Json};
use chrono::Utc;
use conduit_admin_graphql::simple_group::SimpleGroupServices as _;
use conduit_admin_graphql::{
    change_package::{ChangePackageServices as _, ChangeSetAvailableAction as Action},
    change_set::ChangeSetKind,
    governance::GovernanceServices as _,
};
use serde_json::json;
async fn repair_actor(pool: &PgPool, email: &str, scopes: &[&str]) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES($1,'test-hash','activated',$2) RETURNING id")
        .bind(email).bind(sqlx::types::Json(scopes)).fetch_one(pool).await
}
async fn repair_draft(
    service: &PgGovernanceAdapter,
    actor: i64,
    action: &str,
    targets: Vec<ID>,
    payload: serde_json::Value,
) -> Result<conduit_admin_graphql::change_package::ChangeSetDetail, String> {
    let revision = service
        .target_revision(actor, action.into(), targets.clone(), Json(payload.clone()))
        .await?;
    service
        .propose(
            actor,
            gql::ProposeGovernedChangeInput {
                action: action.into(),
                target_ids: targets,
                payload: Json(payload),
                expected_target_revision: revision,
                operation_id: ID(uuid::Uuid::new_v4().to_string()),
                reason: "Repair transaction regression".into(),
            },
        )
        .await
}
async fn repair_approve(
    packages: &crate::wiring_postgres_change_sets::PgChangeSetAdapter,
    maker: i64,
    reviewer: i64,
    draft: conduit_admin_graphql::change_package::ChangeSetDetail,
) -> Result<
    conduit_admin_graphql::change_package::ChangeSetDetail,
    conduit_admin_graphql::change_set::ChangeSetError,
> {
    let pending = packages
        .transition(
            maker,
            draft.summary.id,
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    packages
        .transition(
            reviewer,
            pending.summary.id,
            pending.summary.revision,
            Action::Approve,
            None,
        )
        .await
}

#[tokio::test]
async fn repair_r1_human_observations_and_native_price_copy_recheck_current_side_write_authority()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::wiring_postgres_change_sets::{
        OperationContext,
        packages::{Proposal, stage_observed},
    };
    use crate::wiring_postgres_provider_quota::{
        NewApiModelPricingSnapshot, NewApiPricingSnapshot,
    };
    use conduit_admin_graphql::{
        change_package::OperationOrigin, channel_ext::ChannelExtMutationServices as _,
    };
    use rust_decimal::Decimal;
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let admin = owner(pool, "r1-owner@contract.test").await?;
    let actor = repair_actor(pool, "r1-channel@contract.test", &["write_channels"]).await?;
    let service = PgGovernanceAdapter::new(pool.clone());
    service
        .update_settings(
            admin,
            gql::UpdateGovernanceSettingsInput {
                enabled: false,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let channel:i64=sqlx::query_scalar("INSERT INTO channels(type,name,status,credentials,default_test_model,supported_models,settings) VALUES('openai','r1-observation','disabled','{}','','[\"model-a\"]',$1) RETURNING id")
        .bind(sqlx::types::Json(json!({"billingCurrency":"CNY","rechargeMultiplier":"1"}))).fetch_one(pool).await?;
    let mut snapshot = NewApiPricingSnapshot {
        fetched_at: Utc::now(),
        source_endpoint: "/api/pricing".into(),
        pricing_version: None,
        account_group: None,
        effective_groups: vec![],
        key_count: 1,
        matched_key_count: 1,
        warnings: vec![],
        models: vec![NewApiModelPricingSnapshot {
            provenance: json!({}),
            model_id: "model-a".into(),
            billing_kind: "token".into(),
            quality: "exact".into(),
            group_ratio: None,
            input_per_million: Some(Decimal::ONE),
            output_per_million: Some(Decimal::from(2)),
            cache_read_per_million: Some(Decimal::ZERO),
            cache_write_per_million: Some(Decimal::ONE),
            flat_per_request: None,
            reason: None,
        }],
    };
    // Initialize, manual sync/retry and a human carrying an automatic origin all
    // converge on this observation commit, without acquiring system identity.
    for origin in [
        OperationOrigin::ChannelCreate,
        OperationOrigin::UserSync,
        OperationOrigin::AutomaticSync,
    ] {
        snapshot.fetched_at = Utc::now();
        let context = OperationContext {
            operation_id: uuid::Uuid::new_v4(),
            origin,
            actor_user_id: Some(actor),
        };
        let commit =
            crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot_with_context(
                pool,
                channel,
                "/api/pricing",
                &snapshot,
                &context,
                None,
                None,
            )
            .await?;
        let id = commit.package.ok_or("missing price package")?.id;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
                .bind(id)
                .fetch_one(pool)
                .await?,
            "pending_review"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1"
            )
            .bind(channel)
            .fetch_one(pool)
            .await?,
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT created_by FROM change_sets WHERE id=$1")
                .bind(id)
                .fetch_one(pool)
                .await?,
            actor
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_price_snapshots WHERE channel_id=$1"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        3
    );
    let mapping = Proposal {
        key: "model-a".into(),
        action: "create".into(),
        before: None,
        after: Some(
            json!({"upstreamModelID":"model-a","publicModel":{"modelID":"r1-public","developer":"test","type":"chat","name":"R1","group":"test"},"enabled":true}),
        ),
        source: json!({"source":"observed","channelID":channel}),
        error: None,
    };
    let mut tx = pool.begin().await?;
    stage_observed(
        &mut tx,
        &OperationContext {
            operation_id: uuid::Uuid::new_v4(),
            origin: OperationOrigin::UserSync,
            actor_user_id: Some(actor),
        },
        "model_mapping",
        channel,
        "R1 mapping",
        "1",
        &[mapping.clone()],
        Utc::now(),
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM model_routes")
            .fetch_one(pool)
            .await?,
        0
    );
    sqlx::query(
        "UPDATE users SET scopes='[\"write_channels\",\"write_commercialization\"]' WHERE id=$1",
    )
    .bind(actor)
    .execute(pool)
    .await?;
    let context = OperationContext {
        operation_id: uuid::Uuid::new_v4(),
        origin: OperationOrigin::UserSync,
        actor_user_id: Some(actor),
    };
    let committed =
        crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot_with_context(
            pool,
            channel,
            "/api/pricing",
            &snapshot,
            &context,
            None,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
            .bind(committed.package.ok_or("missing price")?.id)
            .fetch_one(pool)
            .await?,
        "applied"
    );
    let mut tx = pool.begin().await?;
    stage_observed(
        &mut tx,
        &context,
        "model_mapping",
        channel,
        "R1 mapping",
        "2",
        &[mapping],
        Utc::now(),
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM model_routes")
            .fetch_one(pool)
            .await?,
        1
    );

    let adapter = crate::wiring_channel_ext::ChannelExtMutationAdapter::new(
        std::sync::Arc::new(conduit_db::PgChannelRepo::new(pool.clone())),
        pool.clone(),
    );
    let input = |name: &str| conduit_admin_graphql::channel::CreateChannelInput {
        operation_id: None,
        channel_type: conduit_admin_graphql::channel::ChannelType::Openai,
        base_url: None,
        website_url: None,
        quota_currency: None,
        actual_quota_used: None,
        quota_remaining: None,
        name: name.into(),
        credentials: Default::default(),
        supported_models: vec!["model-a".into()],
        manual_models: None,
        auto_sync_supported_models: None,
        auto_sync_model_pattern: None,
        tags: None,
        default_test_model: "".into(),
        policies: None,
        settings: None,
        ordering_weight: None,
        remark: None,
        endpoints: None,
    };
    let revoked = OperationContext {
        operation_id: uuid::Uuid::new_v4(),
        origin: OperationOrigin::ChannelCreate,
        actor_user_id: Some(actor),
    };
    sqlx::query("UPDATE users SET scopes='[\"write_channels\"]' WHERE id=$1")
        .bind(actor)
        .execute(pool)
        .await?;
    let failed = adapter
        .duplicate_channel(Some(actor), &channel.to_string(), input("r1-denied-clone"))
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("FORBIDDEN"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE name='r1-denied-clone'")
            .fetch_one(pool)
            .await?,
        0
    );
    let price_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1",
    )
    .bind(channel)
    .fetch_one(pool)
    .await?;
    snapshot.models[0].input_per_million = Some(Decimal::from(3));
    crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot_with_context(
        pool,
        channel,
        "/api/pricing",
        &snapshot,
        &revoked,
        None,
        None,
    )
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        price_count
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM change_set_events WHERE event_type='publication_blocked'"
        )
        .fetch_one(pool)
        .await?,
        5
    );
    let denied_mapping = Proposal {
        key: "model-a".into(),
        action: "create".into(),
        before: None,
        after: Some(
            json!({"upstreamModelID":"model-a","publicModel":{"modelID":"r1-revoked","developer":"test","type":"chat","name":"Revoked","group":"test"}}),
        ),
        source: json!({"source":"observed","channelID":channel}),
        error: None,
    };
    let mut tx = pool.begin().await?;
    stage_observed(
        &mut tx,
        &revoked,
        "model_mapping",
        channel,
        "Revoked mapping",
        "3",
        &[denied_mapping.clone()],
        Utc::now(),
    )
    .await?;
    tx.commit().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM models WHERE model_id='r1-revoked'")
            .fetch_one(pool)
            .await?,
        0
    );
    sqlx::query(
        "UPDATE users SET scopes='[\"write_channels\",\"write_commercialization\"]' WHERE id=$1",
    )
    .bind(actor)
    .execute(pool)
    .await?;
    let copied = adapter
        .duplicate_channel(
            Some(actor),
            &channel.to_string(),
            input("r1-authorized-clone"),
        )
        .await?;
    let copied_id = conduit_admin_graphql::node::parse_guid(copied.id.as_str())?.id;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_model_prices WHERE channel_id=$1"
        )
        .bind(copied_id)
        .fetch_one(pool)
        .await?,
        1
    );
    // Only the explicit automatic producer retains system publication authority.
    crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot(
        pool,
        channel,
        "/api/pricing",
        &snapshot,
    )
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_model_price_versions WHERE channel_id=$1"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        2
    );
    snapshot.models[0].input_per_million = Some(Decimal::from(4));
    service
        .update_settings(
            admin,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::ProviderPrice, ChangeSetKind::ModelMapping],
                allow_self_review: Some(true),
            },
            "1".into(),
        )
        .await?;
    let on = OperationContext {
        operation_id: uuid::Uuid::new_v4(),
        origin: OperationOrigin::ChannelCreate,
        actor_user_id: Some(actor),
    };
    let staged =
        crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot_with_context(
            pool,
            channel,
            "/api/pricing",
            &snapshot,
            &on,
            None,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
            .bind(staged.package.ok_or("missing reviewed price")?.id)
            .fetch_one(pool)
            .await?,
        "pending_review"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_model_price_versions WHERE channel_id=$1"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        2
    );
    let mut tx = pool.begin().await?;
    let mapping_on = stage_observed(
        &mut tx,
        &on,
        "model_mapping",
        channel,
        "Reviewed mapping",
        "4",
        &[denied_mapping],
        Utc::now(),
    )
    .await?
    .ok_or("missing mapping proposal")?;
    tx.commit().await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
            .bind(mapping_on.id)
            .fetch_one(pool)
            .await?,
        "pending_review"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM model_routes")
            .fetch_one(pool)
            .await?,
        1
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn repair_r2_group_price_create_update_share_domain_checks_and_recheck_review_publish()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let admin = owner(pool, "r2-owner@contract.test").await?;
    owner(pool, "r2-backup-reviewer@contract.test").await?;
    let maker = repair_actor(pool, "r2-maker@contract.test", &["write_groups"]).await?;
    let reviewer = repair_actor(
        pool,
        "r2-reviewer@contract.test",
        &["write_groups", "review_changes"],
    )
    .await?;
    let publisher = repair_actor(
        pool,
        "r2-publisher@contract.test",
        &["write_groups", "publish_changes"],
    )
    .await?;
    let plan:i64=sqlx::query_scalar("INSERT INTO access_plans(name,status,created_at,updated_at) VALUES('R2 access','enabled',now(),now()) RETURNING id").fetch_one(pool).await?;
    let tier:i64=sqlx::query_scalar("INSERT INTO price_tiers(name,status,multiplier_ppm,created_at,updated_at) VALUES('R2 price','enabled',1000000,now(),now()) RETURNING id").fetch_one(pool).await?;
    let group = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO simple_groups(id,name,access_plan_id,price_tier_id,status,created_at,updated_at) VALUES($1,'R2 group',$2,$3,'enabled',now(),now())").bind(&group).bind(plan).bind(tier).execute(pool).await?;
    let governance = PgGovernanceAdapter::new(pool.clone());
    governance
        .update_settings(
            admin,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::RetailPrice],
                allow_self_review: Some(false),
            },
            "0".into(),
        )
        .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    for (action, targets, payload) in [
        (
            "group.price",
            vec![ID(group.clone())],
            json!({"group_id":group,"multiplier_ppm":1500000}),
        ),
        (
            "group.update",
            vec![ID(group.clone())],
            json!({"group_id":group,"multiplier_ppm":1600000}),
        ),
        (
            "group.create",
            vec![],
            json!({"name":"R2 created group","access_plan_id":plan.to_string(),"multiplier_ppm":1700000}),
        ),
    ] {
        sqlx::query("UPDATE users SET scopes='[\"write_groups\"]' WHERE id=$1")
            .bind(maker)
            .execute(pool)
            .await?;
        assert!(
            governance
                .target_revision(maker, action.into(), targets.clone(), Json(payload.clone()))
                .await
                .unwrap_err()
                .contains("FORBIDDEN")
        );
        sqlx::query(
            "UPDATE users SET scopes='[\"write_groups\",\"write_commercialization\"]' WHERE id=$1",
        )
        .bind(maker)
        .execute(pool)
        .await?;
        let draft = repair_draft(&governance, maker, action, targets, payload).await?;
        let pending = packages
            .transition(
                maker,
                draft.summary.id,
                draft.summary.revision,
                Action::Submit,
                None,
            )
            .await?;
        sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"review_changes\"]' WHERE id=$1")
            .bind(reviewer)
            .execute(pool)
            .await?;
        assert!(
            packages
                .transition(
                    reviewer,
                    pending.summary.id.clone(),
                    pending.summary.revision.clone(),
                    Action::Approve,
                    None
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("FORBIDDEN")
        );
        sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"write_commercialization\",\"review_changes\"]' WHERE id=$1").bind(reviewer).execute(pool).await?;
        let approved = packages
            .transition(
                reviewer,
                pending.summary.id,
                pending.summary.revision,
                Action::Approve,
                None,
            )
            .await?;
        sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"publish_changes\"]' WHERE id=$1")
            .bind(publisher)
            .execute(pool)
            .await?;
        assert!(
            packages
                .transition(
                    publisher,
                    approved.summary.id.clone(),
                    approved.summary.revision.clone(),
                    Action::Publish,
                    None
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("FORBIDDEN")
        );
        sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"write_commercialization\",\"publish_changes\"]' WHERE id=$1").bind(publisher).execute(pool).await?;
        let applied = packages
            .transition(
                publisher,
                approved.summary.id,
                approved.summary.revision,
                Action::Publish,
                None,
            )
            .await?;
        assert_eq!(
            applied.summary.status,
            conduit_admin_graphql::change_set::ChangeSetStatus::Applied
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1600000
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT t.multiplier_ppm FROM price_tiers t JOIN simple_groups g ON g.price_tier_id=t.id WHERE g.name='R2 created group'").fetch_one(pool).await?,1700000);
    let draft = repair_draft(
        &governance,
        maker,
        "group.update",
        vec![ID(group.clone())],
        json!({"group_id":group,"multiplier_ppm":1800000}),
    )
    .await?;
    let approved = repair_approve(&packages, maker, reviewer, draft).await?;
    sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"review_changes\"]' WHERE id=$1")
        .bind(reviewer)
        .execute(pool)
        .await?;
    assert!(
        packages
            .transition(
                publisher,
                approved.summary.id.clone(),
                approved.summary.revision.clone(),
                Action::Publish,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("FORBIDDEN")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1600000
    );
    sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"write_commercialization\",\"review_changes\"]' WHERE id=$1").bind(reviewer).execute(pool).await?;
    sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"publish_changes\"]' WHERE id=$1")
        .bind(publisher)
        .execute(pool)
        .await?;
    assert!(
        packages
            .transition(
                publisher,
                approved.summary.id.clone(),
                approved.summary.revision.clone(),
                Action::Publish,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("FORBIDDEN")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1600000
    );
    sqlx::query("UPDATE users SET scopes='[\"write_groups\",\"write_commercialization\",\"publish_changes\"]' WHERE id=$1").bind(publisher).execute(pool).await?;
    packages
        .transition(
            publisher,
            approved.summary.id,
            approved.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1800000
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn repair_r3_group_proposals_bind_price_tier_and_published_access_scope_but_not_consumption()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let maker = owner(pool, "r3-maker@contract.test").await?;
    let reviewer = owner(pool, "r3-reviewer@contract.test").await?;
    let plan:i64=sqlx::query_scalar("INSERT INTO access_plans(name,status,created_at,updated_at) VALUES('R3 access','enabled',now(),now()) RETURNING id").fetch_one(pool).await?;
    let tier:i64=sqlx::query_scalar("INSERT INTO price_tiers(name,status,multiplier_ppm,created_at,updated_at) VALUES('R3 price','enabled',1000000,now(),now()) RETURNING id").fetch_one(pool).await?;
    let group = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO simple_groups(id,name,access_plan_id,price_tier_id,status,created_at,updated_at) VALUES($1,'R3 group',$2,$3,'enabled',now(),now())").bind(&group).bind(plan).bind(tier).execute(pool).await?;
    let project: i64 = sqlx::query_scalar(
        "INSERT INTO projects(name,status) VALUES('R3 member','active') RETURNING id",
    )
    .fetch_one(pool)
    .await?;
    sqlx::query("INSERT INTO simple_group_projects(simple_group_id,project_id,created_at) VALUES($1,$2,now())").bind(&group).bind(project).execute(pool).await?;
    sqlx::query("INSERT INTO project_member_spend_state(project_id,user_id) VALUES($1,$2)")
        .bind(project)
        .bind(maker)
        .execute(pool)
        .await?;
    let governance = PgGovernanceAdapter::new(pool.clone());
    governance
        .update_settings(
            maker,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::RetailPrice],
                allow_self_review: Some(false),
            },
            "0".into(),
        )
        .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    for action in ["group.price", "group.update"] {
        let a = repair_draft(
            &governance,
            maker,
            action,
            vec![ID(group.clone())],
            json!({"group_id":group,"multiplier_ppm":2000000}),
        )
        .await?;
        let a = repair_approve(&packages, maker, reviewer, a).await?;
        let b = repair_draft(
            &governance,
            maker,
            "price_tier.write",
            vec![ID(tier.to_string())],
            json!({"multiplier_ppm":1750000}),
        )
        .await?;
        let b = repair_approve(&packages, maker, reviewer, b).await?;
        packages
            .transition(
                maker,
                b.summary.id,
                b.summary.revision,
                Action::Publish,
                None,
            )
            .await?;
        let failure = packages
            .transition(
                maker,
                a.summary.id.clone(),
                a.summary.revision,
                Action::Publish,
                None,
            )
            .await
            .unwrap_err();
        assert!(failure.to_string().contains("SOURCE_CHANGED"));
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM change_sets WHERE id=$1")
                .bind(a.summary.id.as_str().parse::<i64>()?)
                .fetch_one(pool)
                .await?,
            "superseded"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
                .bind(tier)
                .fetch_one(pool)
                .await?,
            1750000
        );
        let normal = repair_draft(
            &governance,
            maker,
            action,
            vec![ID(group.clone())],
            json!({"group_id":group,"multiplier_ppm":1850000}),
        )
        .await?;
        let normal = repair_approve(&packages, maker, reviewer, normal).await?;
        sqlx::query("UPDATE project_member_spend_state SET spent_micros=spent_micros+100,reserved_micros=50,updated_at=now() WHERE project_id=$1 AND user_id=$2").bind(project).bind(maker).execute(pool).await?;
        packages
            .transition(
                maker,
                normal.summary.id,
                normal.summary.revision,
                Action::Publish,
                None,
            )
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
                .bind(tier)
                .fetch_one(pool)
                .await?,
            1850000
        );
    }
    let a = repair_draft(
        &governance,
        maker,
        "group.update",
        vec![ID(group.clone())],
        json!({"group_id":group,"multiplier_ppm":2100000}),
    )
    .await?;
    let a = repair_approve(&packages, maker, reviewer, a).await?;
    let version:i64=sqlx::query_scalar("INSERT INTO access_plan_versions(access_plan_id,version,status,reference_id,created_at,updated_at) VALUES($1,1,'published','r3-version',now(),now()) RETURNING id").bind(plan).fetch_one(pool).await?;
    sqlx::query("INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES($1,99,now())").bind(version).execute(pool).await?;
    assert!(
        packages
            .transition(
                maker,
                a.summary.id,
                a.summary.revision,
                Action::Publish,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("SOURCE_CHANGED")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1850000
    );
    let scope_changed = repair_draft(
        &governance,
        maker,
        "group.update",
        vec![ID(group.clone())],
        json!({"group_id":group,"multiplier_ppm":2200000}),
    )
    .await?;
    let scope_changed = repair_approve(&packages, maker, reviewer, scope_changed).await?;
    sqlx::query("INSERT INTO access_plan_items(access_plan_version_id,public_model_id,created_at) VALUES($1,100,now())").bind(version).execute(pool).await?;
    assert!(
        packages
            .transition(
                maker,
                scope_changed.summary.id,
                scope_changed.summary.revision,
                Action::Publish,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("SOURCE_CHANGED")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1850000
    );
    db.cleanup().await?;
    Ok(())
}
async fn database()
-> Result<Option<crate::postgres_test_support::IsolatedPostgres>, Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(None);
    };
    Ok(Some(
        crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?,
    ))
}
async fn owner(pool: &PgPool, email: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("INSERT INTO users(email,password,is_owner,status) VALUES($1,'test-hash',true,'activated') RETURNING id").bind(email).fetch_one(pool).await
}
#[tokio::test]
async fn governance_separates_review_publish_and_rolls_back_every_target()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let maker = owner(pool, "maker@contract.test").await?;
    let reviewer = owner(pool, "reviewer@contract.test").await?;
    let mut channels = Vec::new();
    for name in ["before-a", "before-b"] {
        channels.push(sqlx::query_scalar::<_,i64>("INSERT INTO channels(type,name,status,credentials,default_test_model) VALUES('openai',$1,'disabled','{}','') RETURNING id").bind(name).fetch_one(pool).await?);
    }
    let service = PgGovernanceAdapter::new(pool.clone());
    let all = vec![
        ChangeSetKind::ChannelConfiguration,
        ChangeSetKind::ModelSupply,
        ChangeSetKind::GroupAccess,
        ChangeSetKind::IdentityAccess,
        ChangeSetKind::ProjectConfiguration,
        ChangeSetKind::SystemConfiguration,
        ChangeSetKind::ProjectCredit,
    ];
    let settings = service
        .update_settings(
            maker,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: all,
                allow_self_review: Some(false),
            },
            "0".into(),
        )
        .await?;
    assert!(!settings.allow_self_review);
    let denied = sqlx::query("UPDATE channels SET name='bypass' WHERE id=$1")
        .bind(channels[0])
        .execute(pool)
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("APPROVAL_REQUIRED"));
    let ids = channels
        .iter()
        .map(|id| ID(id.to_string()))
        .collect::<Vec<_>>();
    let payload =
        json!({"name":"atomic-conflict","credentials":{"apiKey":"private-contract-secret"}});
    let base = service
        .target_revision(
            maker,
            "channel.write".into(),
            ids.clone(),
            Json(payload.clone()),
        )
        .await?;
    let op = ID(uuid::Uuid::new_v4().to_string());
    let input = gql::ProposeGovernedChangeInput {
        action: "channel.write".into(),
        target_ids: ids,
        payload: Json(payload),
        expected_target_revision: base,
        operation_id: op,
        reason: "Publish an atomic channel update".into(),
    };
    let draft = service.propose(maker, input.clone()).await?;
    let replay = service.propose(maker, input).await?;
    assert_eq!(draft.summary.id, replay.summary.id);
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    let pending = packages
        .transition(
            maker,
            draft.summary.id.clone(),
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    let self_review = packages
        .transition(
            maker,
            pending.summary.id.clone(),
            pending.summary.revision.clone(),
            Action::Approve,
            None,
        )
        .await
        .unwrap_err();
    assert!(self_review.to_string().contains("MAKER_CHECKER_REQUIRED"));
    let approved = packages
        .transition(
            reviewer,
            pending.summary.id.clone(),
            pending.summary.revision,
            Action::Approve,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM channels WHERE id=$1")
            .bind(channels[0])
            .fetch_one(pool)
            .await?,
        "before-a"
    );
    let failed = packages
        .transition(
            maker,
            approved.summary.id.clone(),
            approved.summary.revision,
            Action::Publish,
            None,
        )
        .await
        .unwrap_err();
    assert!(failed.to_string().contains("VALIDATION_FAILED"));
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM channels ORDER BY id")
        .fetch_all(pool)
        .await?;
    assert_eq!(names, vec!["before-a", "before-b"]);
    let public:Vec<String>=sqlx::query_scalar("SELECT after_snapshot::text FROM change_set_items UNION ALL SELECT detail::text FROM change_set_events").fetch_all(pool).await?;
    assert!(
        public
            .iter()
            .all(|s| !s.contains("private-contract-secret") && !s.contains("apiKey"))
    );
    assert!(!failed.to_string().contains("private-contract-secret"));
    db.cleanup().await?;
    Ok(())
}
#[tokio::test]
async fn governance_single_admin_requires_an_explicit_self_review_choice()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "single@contract.test").await?;
    sqlx::query("INSERT INTO systems(key,value) VALUES('system_governance_settings',$1)").bind(json!({"revision":0,"enabled":false,"reviewKinds":[],"allowSelfReview":false,"legacyReviewScopes":false}).to_string()).execute(pool).await?;
    let service = PgGovernanceAdapter::new(pool.clone());
    let input = gql::UpdateGovernanceSettingsInput {
        enabled: true,
        review_kinds: vec![ChangeSetKind::ChannelConfiguration],
        allow_self_review: None,
    };
    assert!(
        service
            .update_settings(actor, input, "0".into())
            .await
            .unwrap_err()
            .contains("SELF_REVIEW_CHOICE_REQUIRED")
    );
    let strict = gql::UpdateGovernanceSettingsInput {
        enabled: true,
        review_kinds: vec![ChangeSetKind::ChannelConfiguration],
        allow_self_review: Some(false),
    };
    assert!(
        service
            .update_settings(actor, strict, "0".into())
            .await
            .unwrap_err()
            .contains("NO_ELIGIBLE_REVIEWER")
    );
    let explicit = gql::UpdateGovernanceSettingsInput {
        enabled: true,
        review_kinds: vec![ChangeSetKind::ChannelConfiguration],
        allow_self_review: Some(true),
    };
    assert!(
        service
            .update_settings(actor, explicit, "0".into())
            .await?
            .allow_self_review
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn governance_price_side_writes_and_supply_import_share_the_transaction_gate()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "side-writes@contract.test").await?;
    let plan:i64=sqlx::query_scalar("INSERT INTO access_plans(name,status,created_at,updated_at) VALUES('test access','enabled',now(),now()) RETURNING id").fetch_one(pool).await?;
    let tier:i64=sqlx::query_scalar("INSERT INTO price_tiers(name,status,multiplier_ppm,created_at,updated_at) VALUES('test price','enabled',1000000,now(),now()) RETURNING id").fetch_one(pool).await?;
    let group:String=sqlx::query_scalar("INSERT INTO simple_groups(id,name,access_plan_id,price_tier_id,status,created_at,updated_at) VALUES($3,'test group',$1,$2,'enabled',now(),now()) RETURNING id").bind(plan).bind(tier).bind(uuid::Uuid::new_v4().to_string()).fetch_one(pool).await?;
    let service = PgGovernanceAdapter::new(pool.clone());
    service
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::RetailPrice, ChangeSetKind::ModelSupply],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let groups = crate::wiring_postgres_simple_group::PgSimpleGroupAdapter::new(pool.clone());
    let denied = groups
        .update_simple_group_price(
            Some(actor),
            conduit_admin_graphql::simple_group::UpdateSimpleGroupPriceInput {
                group_id: ID(group.to_string()),
                multiplier_ppm: 1500000,
            },
        )
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("APPROVAL_REQUIRED"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1000000
    );
    let blocked=sqlx::query("INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) VALUES('openai','imported','disabled','{}','[\"gpt-test\"]','')").execute(pool).await.unwrap_err();
    assert!(blocked.to_string().contains("APPROVAL_REQUIRED"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE name='imported'")
            .fetch_one(pool)
            .await?,
        0
    );
    let payload = json!({"group_id":group.to_string(),"multiplier_ppm":1500000});
    let targets = vec![ID(group.to_string())];
    let revision = service
        .target_revision(
            actor,
            "group.price".into(),
            targets.clone(),
            Json(payload.clone()),
        )
        .await?;
    let draft = service
        .propose(
            actor,
            gql::ProposeGovernedChangeInput {
                action: "group.price".into(),
                target_ids: targets,
                payload: Json(payload),
                expected_target_revision: revision,
                operation_id: ID(uuid::Uuid::new_v4().to_string()),
                reason: "Publish the related price change".into(),
            },
        )
        .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    let pending = packages
        .transition(
            actor,
            draft.summary.id.clone(),
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    let approved = packages
        .transition(
            actor,
            pending.summary.id.clone(),
            pending.summary.revision,
            Action::Approve,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1000000
    );
    packages
        .transition(
            actor,
            approved.summary.id,
            approved.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT multiplier_ppm FROM price_tiers WHERE id=$1")
            .bind(tier)
            .fetch_one(pool)
            .await?,
        1500000
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn governance_native_forms_off_publish_signup_and_mixed_key_batch_are_atomic()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_db::{ApiKeyRepo, PolicyContext, Principal, RequestContext};
    use conduit_http::auth_handlers::SignupService;
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "native-forms@contract.test").await?;
    let project:i64=sqlx::query_scalar("INSERT INTO projects(name,status,description,profiles) VALUES('existing','active','','{}') RETURNING id").fetch_one(pool).await?;
    let role:i64=sqlx::query_scalar("INSERT INTO roles(name,level,project_id,scopes) VALUES('reader','project',$1,'[\"read_requests\"]') RETURNING id").bind(project).fetch_one(pool).await?;
    let service = PgGovernanceAdapter::new(pool.clone());
    service
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: false,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let payload = json!({"email":"new-native@contract.test","password":"Password123!","avatar":"https://example.invalid/avatar","projectIDs":[project.to_string()],"roleIDs":[role.to_string()]});
    let base = service
        .target_revision(
            actor,
            "user.create_form".into(),
            vec![],
            Json(payload.clone()),
        )
        .await?;
    let draft = service
        .propose(
            actor,
            gql::ProposeGovernedChangeInput {
                action: "user.create_form".into(),
                target_ids: vec![],
                payload: Json(payload),
                expected_target_revision: base,
                operation_id: ID(uuid::Uuid::new_v4().to_string()),
                reason: "Create a user with its membership and role".into(),
            },
        )
        .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    assert!(draft.available_actions.contains(&Action::Publish));
    packages
        .transition(
            actor,
            draft.summary.id,
            draft.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    let member: i64 =
        sqlx::query_scalar("SELECT id FROM users WHERE email='new-native@contract.test'")
            .fetch_one(pool)
            .await?;
    let stored: String = sqlx::query_scalar("SELECT password FROM users WHERE id=$1")
        .bind(member)
        .fetch_one(pool)
        .await?;
    assert_ne!(stored, "Password123!");
    assert!(conduit_auth::password::verify_password_bcrypt_hex(
        "Password123!",
        &stored
    )?);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM user_projects WHERE user_id=$1 AND project_id=$2"
        )
        .bind(member)
        .bind(project)
        .fetch_one(pool)
        .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM user_roles WHERE user_id=$1 AND role_id=$2"
        )
        .bind(member)
        .bind(role)
        .fetch_one(pool)
        .await?,
        1
    );
    let own:i64=sqlx::query_scalar("INSERT INTO api_keys(user_id,project_id,name,key,\"type\",status,scopes) VALUES($1,$2,'own','test-own','user','enabled','[\"read_channels\",\"write_requests\"]') RETURNING id").bind(actor).bind(project).fetch_one(pool).await?;
    let foreign:i64=sqlx::query_scalar("INSERT INTO api_keys(user_id,project_id,name,key,\"type\",status,scopes) VALUES($1,$2,'foreign','test-foreign','user','enabled','[\"read_channels\",\"write_requests\"]') RETURNING id").bind(member).bind(project).fetch_one(pool).await?;
    sqlx::query(
        "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES($1,$2,true,'[]')",
    )
    .bind(actor)
    .bind(project)
    .execute(pool)
    .await?;
    service
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![
                    ChangeSetKind::IdentityAccess,
                    ChangeSetKind::ProjectConfiguration,
                ],
                allow_self_review: Some(true),
            },
            "1".into(),
        )
        .await?;
    let direct=sqlx::query("INSERT INTO users(email,password,is_owner,scopes) VALUES('bypass@contract.test','hash',false,'[]')").execute(pool).await.unwrap_err();
    assert!(direct.to_string().contains("APPROVAL_REQUIRED"));
    let mut principal = Principal::test();
    principal.subject_id = actor.to_string();
    let mut ctx = RequestContext::new(PolicyContext::new(principal));
    ctx.project_id = Some(project.to_string());
    let repo = conduit_db::PgApiKeyRepo::new(pool.clone());
    assert!(
        repo.update_api_key_status_batch(
            &ctx,
            &project.to_string(),
            &[own.to_string(), foreign.to_string()],
            "disabled"
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("APPROVAL_REQUIRED")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM api_keys WHERE id=ANY($1) AND status='enabled'"
        )
        .bind(vec![own, foreign])
        .fetch_one(pool)
        .await?,
        2
    );
    repo.update_api_key_status_batch(&ctx, &project.to_string(), &[own.to_string()], "disabled")
        .await?;
    sqlx::query("INSERT INTO systems(key,value) VALUES('system_initialized','true') ON CONFLICT(key) DO UPDATE SET value='true'").execute(pool).await?;
    let signup = crate::wiring_postgres_auth::PgSignupService::new(pool.clone(), 4);
    let registered = signup
        .register_user(conduit_http::auth_handlers::SignUpRequest {
            email: "signup@contract.test".into(),
            password: "Password123!".into(),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("signup failed: {e:?}"))?;
    let personal_owner:i64=sqlx::query_scalar("SELECT owner_user_id FROM project_commercial_profiles WHERE owner_user_id=$1 AND account_type='personal'").bind(registered.id).fetch_one(pool).await?;
    assert_eq!(personal_owner, registered.id);
    db.cleanup().await?;
    Ok(())
}

async fn propose_native(
    pool: &PgPool,
    actor: i64,
    action: &str,
    targets: Vec<ID>,
    payload: serde_json::Value,
) -> Result<conduit_admin_graphql::change_package::ChangeSetDetail, Box<dyn std::error::Error>> {
    let service = PgGovernanceAdapter::new(pool.clone());
    let base = service
        .target_revision(actor, action.into(), targets.clone(), Json(payload.clone()))
        .await?;
    Ok(service
        .propose(
            actor,
            gql::ProposeGovernedChangeInput {
                action: action.into(),
                target_ids: targets,
                payload: Json(payload),
                expected_target_revision: base,
                operation_id: ID(uuid::Uuid::new_v4().to_string()),
                reason: "Verify native publication behavior".into(),
            },
        )
        .await?)
}
async fn publish_native(
    pool: &PgPool,
    actor: i64,
    action: &str,
    payload: serde_json::Value,
    review: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut d = propose_native(pool, actor, action, vec![], payload).await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    if review {
        d = packages
            .transition(
                actor,
                d.summary.id,
                d.summary.revision,
                Action::Submit,
                None,
            )
            .await?;
        d = packages
            .transition(
                actor,
                d.summary.id,
                d.summary.revision,
                Action::Approve,
                None,
            )
            .await?;
    }
    packages
        .transition(
            actor,
            d.summary.id,
            d.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    Ok(())
}
fn provider_input() -> Vec<conduit_admin_graphql::model_ext::SaveChannelModelPriceInput> {
    use conduit_admin_graphql::{
        model_ext as m, request_usage::PriceItemCode as C, scalars::DecimalScalar,
    };
    vec![m::SaveChannelModelPriceInput {
        model_id: "gpt-priced".into(),
        currency_code: "CNY".into(),
        price: m::ModelPriceInput {
            items: [
                C::PromptTokens,
                C::CompletionTokens,
                C::PromptCachedTokens,
                C::PromptWriteCachedTokens,
            ]
            .into_iter()
            .map(|code| m::ModelPriceItemInput {
                item_code: code,
                pricing: m::PricingInput {
                    mode: m::PricingMode::UsagePerUnit,
                    flat_fee: None,
                    usage_per_unit: Some(DecimalScalar(rust_decimal::Decimal::ONE)),
                    usage_tiered: None,
                },
                prompt_write_cache_variants: None,
            })
            .collect(),
        },
    }]
}
#[tokio::test]
async fn lite_complete_native_price_off_and_settings_publish_without_secret_exposure()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "settings-price@contract.test").await?;
    let governance = PgGovernanceAdapter::new(pool.clone());
    governance
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: false,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let channel:i64=sqlx::query_scalar("INSERT INTO channels(type,name,credentials,default_test_model,settings) VALUES('openai','native price','{}','gpt-priced','{}') RETURNING id").fetch_one(pool).await?;
    crate::wiring_postgres_change_sets::packages::save_native_provider(
        pool,
        actor,
        channel,
        provider_input(),
    )
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM channel_model_prices WHERE channel_id=$1 AND deleted_at=0"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        1
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM channel_model_price_versions WHERE channel_id=$1 AND status='active'").bind(channel).fetch_one(pool).await?,1);
    let versions = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM channel_model_price_versions WHERE channel_id=$1",
    )
    .bind(channel)
    .fetch_one(pool)
    .await?;
    crate::wiring_postgres_change_sets::packages::save_native_provider(
        pool,
        actor,
        channel,
        provider_input(),
    )
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM channel_model_price_versions WHERE channel_id=$1"
        )
        .bind(channel)
        .fetch_one(pool)
        .await?,
        versions
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pricing_change_audits WHERE operation='apply_provider_price_change_set'").fetch_one(pool).await?,1);
    let copy:i64=sqlx::query_scalar("INSERT INTO channels(type,name,credentials,default_test_model,settings) VALUES('openai','copied price','{}','gpt-priced','{}') RETURNING id").fetch_one(pool).await?;
    let mut tx = pool.begin().await?;
    crate::wiring_governance::lock_policy(&mut tx).await?;
    crate::wiring_postgres_change_sets::publish_copied_provider_prices(
        &mut tx, actor, None, channel, copy, "copy",
    )
    .await?;
    tx.commit().await?;
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM channel_model_price_versions WHERE channel_id=$1 AND status='active'").bind(copy).fetch_one(pool).await?,1);
    let backup = json!({"enabled":false,"frequency":"daily","include_channels":true,"include_models":true,"include_api_keys":true,"include_model_prices":true,"include_usage_stats":false,"include_request_logs":false,"data_storage_id":0,"retention_days":7,"last_backup_at":"2026-01-01T00:00:00Z","last_backup_error":"old observation"});
    sqlx::query("INSERT INTO systems(key,value) VALUES('system_auto_backup_settings',$1)")
        .bind(backup.to_string())
        .execute(pool)
        .await?;
    governance
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![
                    ChangeSetKind::SystemConfiguration,
                    ChangeSetKind::ProviderPrice,
                ],
                allow_self_review: Some(true),
            },
            "1".into(),
        )
        .await?;
    publish_native(
        pool,
        actor,
        "system.general_form",
        json!({"timezone":"UTC"}),
        true,
    )
    .await?;
    publish_native(
        pool,
        actor,
        "system.proxy_save",
        json!({"name":"test","url":"http://proxy.invalid","password":"private-proxy-test"}),
        true,
    )
    .await?;
    publish_native(
        pool,
        actor,
        "system.brand_form",
        json!({"brandName":"Test conduit","title":"Test console","brandLogo":"logo"}),
        true,
    )
    .await?;
    publish_native(
        pool,
        actor,
        "system.quota_form",
        json!({"enabled":true,"mode":"DE_PRIORITIZE"}),
        true,
    )
    .await?;
    publish_native(
        pool,
        actor,
        "system.backup_form",
        json!({"enabled":false,"retentionDays":9}),
        true,
    )
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT value FROM systems WHERE key='system_brand_name'")
            .fetch_one(pool)
            .await?,
        "Test conduit"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT value FROM systems WHERE key='system_title'")
            .fetch_one(pool)
            .await?,
        "Test console"
    );
    let backup: String =
        sqlx::query_scalar("SELECT value FROM systems WHERE key='system_auto_backup_settings'")
            .fetch_one(pool)
            .await?;
    let backup: serde_json::Value = serde_json::from_str(&backup)?;
    assert_eq!(backup["retention_days"], 9);
    // A caller cannot forge last-run observations with an ordinary settings proposal.
    assert_eq!(backup["last_backup_at"], "2026-01-01T00:00:00Z");
    assert_eq!(backup["last_backup_error"], "old observation");
    let public:Vec<String>=sqlx::query_scalar("SELECT after_snapshot::text FROM change_set_items UNION ALL SELECT detail::text FROM change_set_events").fetch_all(pool).await?;
    assert!(public.iter().all(|v| !v.contains("private-proxy-test")));
    let bad = propose_native(
        pool,
        actor,
        "system.general_form",
        vec![],
        json!({"creditsPerAccountingUnit":"0"}),
    )
    .await;
    assert!(bad.is_err());
    db.cleanup().await?;
    Ok(())
}
#[tokio::test]
async fn lite_complete_reviewer_domain_revocation_and_health_restore_are_enforced()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let administrator = owner(pool, "governance-owner@contract.test").await?;
    owner(pool, "backup-owner@contract.test").await?;
    let maker:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('domain-maker@contract.test','hash','activated','[\"write_settings\"]') RETURNING id").fetch_one(pool).await?;
    let reviewer:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('domain-reviewer@contract.test','hash','activated','[\"write_settings\",\"review_changes\"]') RETURNING id").fetch_one(pool).await?;
    let publisher:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('domain-publisher@contract.test','hash','activated','[\"write_settings\",\"publish_changes\"]') RETURNING id").fetch_one(pool).await?;
    let governance = PgGovernanceAdapter::new(pool.clone());
    governance
        .update_settings(
            administrator,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![
                    ChangeSetKind::SystemConfiguration,
                    ChangeSetKind::ChannelConfiguration,
                ],
                allow_self_review: Some(false),
            },
            "0".into(),
        )
        .await?;
    let denied = propose_native(
        pool,
        maker,
        "channel.write",
        vec![],
        json!({"name":"outside","type":"openai","credentials":{}}),
    )
    .await;
    assert!(denied.unwrap_err().to_string().contains("FORBIDDEN"));
    let draft = propose_native(
        pool,
        maker,
        "system.brand_form",
        vec![],
        json!({"title":"after review"}),
    )
    .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    let pending = packages
        .transition(
            maker,
            draft.summary.id,
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    assert!(
        packages
            .transition(
                maker,
                pending.summary.id.clone(),
                pending.summary.revision.clone(),
                Action::Approve,
                None
            )
            .await
            .is_err()
    );
    let approved = packages
        .transition(
            reviewer,
            pending.summary.id,
            pending.summary.revision,
            Action::Approve,
            None,
        )
        .await?;
    sqlx::query("UPDATE users SET scopes='[\"review_changes\"]' WHERE id=$1")
        .bind(reviewer)
        .execute(pool)
        .await?;
    assert!(
        packages
            .transition(
                publisher,
                approved.summary.id.clone(),
                approved.summary.revision.clone(),
                Action::Publish,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("FORBIDDEN")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM systems WHERE key='system_title' AND value='after review'"
        )
        .fetch_one(pool)
        .await?,
        0
    );
    sqlx::query("UPDATE users SET scopes='[\"write_settings\",\"review_changes\"]' WHERE id=$1")
        .bind(reviewer)
        .execute(pool)
        .await?;
    packages
        .transition(
            publisher,
            approved.summary.id,
            approved.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    // Trusted automatic health authority can add restrictions, never restore access.
    let draft=propose_native(pool,administrator,"channel.write",vec![],json!({"type":"openai","name":"health","status":"enabled","default_test_model":"","credentials":{},"settings":{}})).await?;
    let pending = packages
        .transition(
            administrator,
            draft.summary.id,
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    let backup_owner: i64 =
        sqlx::query_scalar("SELECT id FROM users WHERE email='backup-owner@contract.test'")
            .fetch_one(pool)
            .await?;
    let approved = packages
        .transition(
            backup_owner,
            pending.summary.id,
            pending.summary.revision,
            Action::Approve,
            None,
        )
        .await?;
    packages
        .transition(
            administrator,
            approved.summary.id,
            approved.summary.revision,
            Action::Publish,
            None,
        )
        .await?;
    let channel: i64 = sqlx::query_scalar("SELECT id FROM channels WHERE name='health'")
        .fetch_one(pool)
        .await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('conduit.health_restriction','authorized',true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE channels SET status='disabled',disabled_api_keys='[\"opaque-key\"]' WHERE id=$1",
    )
    .bind(channel)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('conduit.health_restriction','authorized',true)")
        .execute(&mut *tx)
        .await?;
    assert!(
        sqlx::query("UPDATE channels SET status='enabled',disabled_api_keys='[]' WHERE id=$1")
            .bind(channel)
            .execute(&mut *tx)
            .await
            .unwrap_err()
            .to_string()
            .contains("APPROVAL_REQUIRED")
    );
    tx.rollback().await?;
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn lite_complete_module_gates_stop_new_management_but_preserve_issued_credit()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_admin_graphql::{
        billing::{BillingServices as _, CreateCreditRedemptionCodesInput, CreditRedemptionActor},
        operations::OperationsServices as _,
    };
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "module-owner@contract.test").await?;
    PgGovernanceAdapter::new(pool.clone())
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: false,
                review_kinds: vec![],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    let project:i64=sqlx::query_scalar("INSERT INTO projects(name,status,description,profiles) VALUES('module project','active','','{}') RETURNING id").fetch_one(pool).await?;
    sqlx::query(
        "INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES($1,$2,true,'[]')",
    )
    .bind(actor)
    .bind(project)
    .execute(pool)
    .await?;
    let issued = crate::wiring_postgres_redemption::create_codes(
        pool,
        CreditRedemptionActor {
            actor_type: "user".into(),
            actor_id: Some(actor.to_string()),
        },
        CreateCreditRedemptionCodesInput {
            amount: "2".into(),
            quantity: 1,
            max_redemptions: 1,
            expires_at: None,
            description: None,
        },
    )
    .await?;
    let overrides = std::collections::BTreeMap::from_iter(
        [
            "operations_reports",
            "automatic_model_sync",
            "automatic_provider_observation",
            "redemption_issuance",
            "subscription_management",
        ]
        .map(|id| (id, json!({"visibility":"INHERIT","execution":"OFF"}))),
    );
    sqlx::query("INSERT INTO systems(key,value) VALUES('system_product_experience_settings',$1)")
        .bind(json!({"mode":"simple","revision":1,"overrides":overrides}).to_string())
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO channels(type,name,status,base_url,credentials,auto_sync_supported_models,default_test_model) VALUES('openai','disabled feature target','enabled','http://127.0.0.1:1','{}',true,'')").execute(pool).await?;
    // Disabled workers return before claiming work or making any provider request.
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::wiring_postgres_channel_model_sync::PgChannelModelSyncAdapter::new(pool.clone())
            .run(),
    )
    .await??;
    let system = std::sync::Arc::new(conduit_services::SystemService::from_system_repo(
        std::sync::Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
        std::sync::Arc::new(conduit_cache::NoopCache::new()),
    ));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        crate::wiring_postgres_provider_quota::PgProviderQuotaAdapter::new(pool.clone(), system)
            .check(false),
    )
    .await??;
    assert!(
        crate::wiring_postgres_operations::PgOperationsAdapter::new(pool.clone())
            .operations_ledger(7)
            .await
            .unwrap_err()
            .to_string()
            .contains("FEATURE_DISABLED")
    );
    let billing = crate::wiring_postgres_billing::PgBillingAdapter::new(pool.clone());
    assert!(
        billing
            .create_subscription_plan(
                conduit_admin_graphql::billing::CreateSubscriptionPlanInput {
                    name: "disabled plan".into(),
                    interval_unit: conduit_admin_graphql::billing::SubscriptionIntervalUnit::Month,
                    interval_count: Some(1),
                    access_plan_ids: vec![],
                    quota_rules: vec![conduit_admin_graphql::billing::SubscriptionQuotaRuleInput {
                        id: None,
                        name: "General".into(),
                        quota_class: conduit_admin_graphql::billing::QuotaClass::General,
                        allowance: "1".into(),
                        rollover_mode: None,
                        rollover_cap: None,
                        carryover_days: None,
                        access_plan_ids: vec![]
                    }]
                }
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("FEATURE_DISABLED")
    );
    assert!(
        crate::wiring_postgres_redemption::create_codes(
            pool,
            CreditRedemptionActor {
                actor_type: "user".into(),
                actor_id: Some(actor.to_string())
            },
            CreateCreditRedemptionCodesInput {
                amount: "2".into(),
                quantity: 1,
                max_redemptions: 1,
                expires_at: None,
                description: None
            }
        )
        .await
        .err()
        .expect("issuance must be disabled")
        .to_string()
        .contains("FEATURE_DISABLED")
    );
    crate::wiring_postgres_redemption::redeem_code(
        pool,
        CreditRedemptionActor {
            actor_type: "user".into(),
            actor_id: Some(actor.to_string()),
        },
        &actor.to_string(),
        &project.to_string(),
        &issued.codes[0].code,
    )
    .await?;
    assert_eq!(
        billing
            .my_project_funds(&actor.to_string(), &project.to_string(), None, None)
            .await?
            .project_credit_available,
        "2"
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn lite_complete_product_execution_form_preserves_owner_revision_and_named_sql_gate()
-> Result<(), Box<dyn std::error::Error>> {
    use conduit_admin_graphql::product_experience::{
        ProductExperienceServices as _, ProductMode, UpdateProductExperienceSettingsInput,
    };
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let actor = owner(pool, "product-owner@contract.test").await?;
    let settings_user:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('settings-user@contract.test','hash','activated','[\"write_settings\",\"review_changes\",\"publish_changes\"]') RETURNING id").fetch_one(pool).await?;
    sqlx::query("INSERT INTO systems(key,value) VALUES('system_product_experience_settings',$1)")
        .bind(json!({"mode":"simple","revision":7,"overrides":{"automatic_model_sync":{"visibility":"ON","execution":"OFF"}}}).to_string()).execute(pool).await?;
    PgGovernanceAdapter::new(pool.clone())
        .update_settings(
            actor,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::SystemConfiguration],
                allow_self_review: Some(true),
            },
            "0".into(),
        )
        .await?;
    // An equal array [OFF] for a different named module changes runtime authority.
    let denied=sqlx::query("UPDATE systems SET value=$1 WHERE key='system_product_experience_settings'")
        .bind(json!({"mode":"simple","revision":8,"overrides":{"subscription_management":{"execution":"OFF"}}}).to_string())
        .execute(pool).await.unwrap_err();
    assert!(denied.to_string().contains("APPROVAL_REQUIRED"));
    let system = std::sync::Arc::new(conduit_services::SystemService::from_system_repo(
        std::sync::Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
        std::sync::Arc::new(conduit_cache::NoopCache::new()),
    ));
    let product = crate::wiring_product_experience::ProductExperienceAdapter::new(system)
        .with_pool(pool.clone());
    // Presentation changes remain usable while runtime changes require a package.
    let switched = product
        .update_settings(UpdateProductExperienceSettingsInput {
            mode: ProductMode::Enterprise,
            expected_revision: Some("7".into()),
            overrides: None,
        })
        .await?;
    assert_eq!(switched.revision, "8");
    assert!(
        !crate::wiring_product_experience::feature_enabled(pool, "automatic_model_sync").await?
    );
    let payload = json!({"mode":"SIMPLE","expectedRevision":"8","overrides":[{"id":"automatic_model_sync","visibility":"OFF","execution":"ON"}]});
    assert!(
        propose_native(
            pool,
            settings_user,
            "system.product_experience_form",
            vec![],
            payload.clone()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Owner")
    );
    let draft = propose_native(
        pool,
        actor,
        "system.product_experience_form",
        vec![],
        payload,
    )
    .await?;
    assert!(
        !crate::wiring_product_experience::feature_enabled(pool, "automatic_model_sync").await?
    );
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    let pending = packages
        .transition(
            actor,
            draft.summary.id,
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    let applied = packages
        .transition(
            actor,
            pending.summary.id,
            pending.summary.revision,
            Action::ApproveApply,
            None,
        )
        .await?;
    assert_eq!(
        applied.summary.status,
        conduit_admin_graphql::change_set::ChangeSetStatus::Applied
    );
    let published = product.settings().await?;
    assert_eq!(published.mode, ProductMode::Simple);
    assert_eq!(published.revision, "9");
    let sync = published
        .features
        .iter()
        .find(|f| f.id == "automatic_model_sync")
        .unwrap();
    assert!(sync.enabled);
    assert!(!sync.visible);
    assert!(
        crate::wiring_product_experience::feature_enabled(pool, "subscription_management").await?
    );
    db.cleanup().await?;
    Ok(())
}

#[tokio::test]
async fn lite_complete_approve_apply_requires_both_duties_and_records_two_steps()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(db) = database().await? else {
        return Ok(());
    };
    let pool = &db.pool;
    let maker = owner(pool, "combined-maker@contract.test").await?;
    let reviewer = owner(pool, "combined-owner@contract.test").await?;
    let review_only:i64=sqlx::query_scalar("INSERT INTO users(email,password,status,scopes) VALUES('review-only@contract.test','hash','activated','[\"write_settings\",\"review_changes\"]') RETURNING id").fetch_one(pool).await?;
    PgGovernanceAdapter::new(pool.clone())
        .update_settings(
            maker,
            gql::UpdateGovernanceSettingsInput {
                enabled: true,
                review_kinds: vec![ChangeSetKind::SystemConfiguration],
                allow_self_review: Some(false),
            },
            "0".into(),
        )
        .await?;
    let draft = propose_native(
        pool,
        maker,
        "system.brand_form",
        vec![],
        json!({"title":"combined publication"}),
    )
    .await?;
    let packages = crate::wiring_postgres_change_sets::PgChangeSetAdapter::new(pool.clone());
    let pending = packages
        .transition(
            maker,
            draft.summary.id,
            draft.summary.revision,
            Action::Submit,
            None,
        )
        .await?;
    assert!(
        packages
            .transition(
                review_only,
                pending.summary.id.clone(),
                pending.summary.revision.clone(),
                Action::ApproveApply,
                None
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("FORBIDDEN")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM systems WHERE key='system_title'")
            .fetch_one(pool)
            .await?,
        0
    );
    let applied = packages
        .transition(
            reviewer,
            pending.summary.id,
            pending.summary.revision,
            Action::ApproveApply,
            None,
        )
        .await?;
    assert_eq!(
        applied.summary.status,
        conduit_admin_graphql::change_set::ChangeSetStatus::Applied
    );
    let events:Vec<String>=sqlx::query_scalar("SELECT event_type FROM change_set_events WHERE change_set_id=$1 AND event_type IN ('approved','published') ORDER BY id").bind(applied.summary.id.as_str().parse::<i64>()?).fetch_all(pool).await?;
    assert_eq!(events, vec!["approved", "published"]);
    db.cleanup().await?;
    Ok(())
}

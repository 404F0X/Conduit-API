//! Controlled production lock-order regressions; advisory keys require a private database.
use crate::wiring_postgres_billing::PgBillingAdapter;
use conduit_admin_graphql::billing::{self as gql, BillingServices};
use sqlx::{PgPool, postgres::PgPoolOptions};
type Error = Box<dyn std::error::Error>;

struct Worker<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn connection(pool: &PgPool) -> Result<(PgPool, i32), Error> {
    let worker = PgPoolOptions::new()
        .max_connections(1)
        .connect_with((*pool.connect_options()).clone())
        .await?;
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&worker)
        .await?;
    Ok((worker, pid))
}
async fn blocked_by(pool: &PgPool, waiter: i32, holder: i32) -> Result<(), Error> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT $2=ANY(pg_blocking_pids($1))")
                .bind(waiter)
                .bind(holder)
                .fetch_one(pool)
                .await?;
            if blocked {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}
fn rules() -> Vec<gql::SubscriptionQuotaRuleInput> {
    vec![gql::SubscriptionQuotaRuleInput {
        id: None,
        name: "general".into(),
        quota_class: gql::QuotaClass::General,
        allowance: "10".into(),
        rollover_mode: Some(gql::RolloverMode::None),
        rollover_cap: None,
        carryover_days: None,
        access_plan_ids: vec![],
    }]
}
fn assignment(plan: &str, key: &str) -> gql::AssignUserSubscriptionInput {
    gql::AssignUserSubscriptionInput {
        user_id: "1".into(),
        project_id: "1".into(),
        plan_id: plan.into(),
        idempotency_key: key.into(),
        auto_renew: Some(true),
        interval_unit: None,
        interval_count: None,
    }
}

#[tokio::test]
async fn subscription_mutations_take_governance_before_group_and_row_locks() -> Result<(), Error> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    crate::postgres_test_support::run_private_database(&dsn, |pool| async move {
        sqlx::query("INSERT INTO users(id,email,password,status,is_owner) VALUES(1,'lock-order@fixture.invalid','fixture','activated',true)").execute(&pool).await?;
        sqlx::query("INSERT INTO projects(id,name,status,profiles) VALUES(1,'lock-order','active','{}')").execute(&pool).await?;
        sqlx::query("INSERT INTO user_projects(user_id,project_id,is_owner,scopes) VALUES(1,1,true,'[]')").execute(&pool).await?;
        let adapter = PgBillingAdapter::new(pool.clone());
        let plan = adapter.create_subscription_plan(gql::CreateSubscriptionPlanInput { name: "lock-order".into(), interval_unit: gql::SubscriptionIntervalUnit::Month, interval_count: Some(1), access_plan_ids: vec![], quota_rules: rules() }).await?;
        let initial = adapter.assign_user_subscription(assignment(plan.id.as_str(), "initial")).await?;
        let sub: i64 = initial.id.as_str().rsplit('/').next().ok_or("subscription ID")?.parse()?;
        let plan_id: i64 = plan.id.as_str().rsplit('/').next().ok_or("plan ID")?.parse()?;
        let (first_pool, first_pid) = connection(&pool).await?;
        let (second_pool, second_pid) = connection(&pool).await?;
        // Holding S forces assignment to expose whether it acquired G first.
        let mut blocker = pool.begin().await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *blocker).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('conduit.simple_groups',0))").execute(&mut *blocker).await?;
        let first_adapter = PgBillingAdapter::new(first_pool.clone());
        let input = assignment(plan.id.as_str(), "concurrent");
        let mut first = Worker(tokio::spawn(async move { first_adapter.assign_user_subscription(input).await }));
        blocked_by(&pool, first_pid, blocker_pid).await?;
        let second_adapter = PgBillingAdapter::new(second_pool.clone());
        let initial_id = initial.id.to_string();
        let mut second = Worker(tokio::spawn(async move { second_adapter.refresh_subscription_allowance(&initial_id).await }));
        blocked_by(&pool, second_pid, first_pid).await?;
        blocker.rollback().await?;
        (&mut first.0).await??;
        (&mut second.0).await??;
        // Row blockers establish that update/cancel own G before touching rows.
        for cancel in [false, true] {
            let mut blocker = pool.begin().await?;
            let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *blocker).await?;
            if cancel { sqlx::query("SELECT id FROM user_subscriptions WHERE id=$1 FOR UPDATE").bind(sub).execute(&mut *blocker).await?; }
            else { sqlx::query("SELECT id FROM subscription_plans WHERE id=$1 FOR UPDATE").bind(plan_id).execute(&mut *blocker).await?; }
            let first_adapter = PgBillingAdapter::new(first_pool.clone());
            let target = initial.id.to_string();
            let plan_key = plan.id.clone();
            let mut first = Worker(tokio::spawn(async move {
                if cancel { first_adapter.cancel_user_subscription(&target).await.map(|_| ()) }
                else { first_adapter.update_subscription_plan(gql::UpdateSubscriptionPlanInput { id: plan_key, name: "updated".into(), interval_unit: gql::SubscriptionIntervalUnit::Month, interval_count: 1, access_plan_ids: vec![], quota_rules: rules(), status: gql::BillingStatus::Enabled }).await.map(|_| ()) }
            }));
            blocked_by(&pool, first_pid, blocker_pid).await?;
            let second_adapter = PgBillingAdapter::new(second_pool.clone());
            let mut second = Worker(tokio::spawn(async move { second_adapter.process_due_subscriptions().await }));
            blocked_by(&pool, second_pid, first_pid).await?;
            blocker.rollback().await?;
            (&mut first.0).await??;
            (&mut second.0).await??;
        }
        first_pool.close().await;
        second_pool.close().await;
        Ok(())
    }).await
}

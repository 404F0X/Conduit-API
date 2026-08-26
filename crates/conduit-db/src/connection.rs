//! Live PostgreSQL pool construction and migration bootstrap.

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPool, PgSslMode};

use crate::migrate::{
    LATEST_SCHEMA_VERSION, MigrationOutcome, MigrationRunnerError,
    required_postgres_schema_versions, run_migrations_postgres,
};
use crate::pool::{DatabaseConfig, pool_options};

/// Live PostgreSQL connection set. The master is the only pool used for writes
/// and transactions; the optional read pool is reserved for explicitly
/// eventual-consistency queries.
#[derive(Clone)]
pub struct PostgresPools {
    master: PgPool,
    read: Option<PgPool>,
    fallback_on_replica_failure: bool,
}

impl PostgresPools {
    pub fn new(master: PgPool, read: Option<PgPool>, fallback_on_replica_failure: bool) -> Self {
        Self {
            master,
            read,
            fallback_on_replica_failure,
        }
    }

    pub fn master(&self) -> &PgPool {
        &self.master
    }

    pub fn master_clone(&self) -> PgPool {
        self.master.clone()
    }

    pub fn read(&self) -> Option<&PgPool> {
        self.read.as_ref()
    }

    pub fn fallback_on_replica_failure(&self) -> bool {
        self.fallback_on_replica_failure
    }

    pub async fn read_schema_version(&self) -> Result<Option<String>, sqlx::Error> {
        let Some(read) = self.read.as_ref() else {
            return Ok(None);
        };
        sqlx::query_scalar("SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1")
            .fetch_optional(read)
            .await
    }

    pub fn disable_read(&mut self) {
        self.read = None;
    }
}

pub async fn connect_postgres_pools(
    master: &DatabaseConfig,
    read_dsn: Option<&str>,
    read_max_connections: u32,
    read_max_idle_connections: u32,
    fallback_on_replica_failure: bool,
) -> Result<PostgresPools, sqlx::Error> {
    let master_pool = connect_postgres(master).await?;
    let read_pool = match read_dsn.map(str::trim).filter(|dsn| !dsn.is_empty()) {
        Some(dsn) => {
            let mut replica = master.clone();
            replica.dsn = dsn.to_owned();
            if read_max_connections > 0 {
                replica.max_connections = read_max_connections;
            }
            if read_max_idle_connections > 0 {
                replica.min_connections = read_max_idle_connections.min(replica.max_connections);
            }
            match connect_postgres(&replica).await {
                Ok(pool) => Some(pool),
                Err(_error) if fallback_on_replica_failure => None,
                Err(error) => return Err(error),
            }
        }
        None => None,
    };

    Ok(PostgresPools::new(
        master_pool,
        read_pool,
        fallback_on_replica_failure,
    ))
}

/// Build an eagerly connected PostgreSQL pool.
pub async fn connect_postgres(config: &DatabaseConfig) -> Result<PgPool, sqlx::Error> {
    let mut options = PgConnectOptions::from_str(&config.dsn)?;
    if !config.dsn.to_ascii_lowercase().contains("sslmode=") {
        options = options.ssl_mode(PgSslMode::Prefer);
    }
    options = options.statement_cache_capacity(256);

    pool_options::build_pool_options::<sqlx::Postgres>(config)
        .after_connect(|connection, _meta| {
            Box::pin(async move {
                sqlx::query("SET TIME ZONE 'UTC'")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET application_name = 'conduit'")
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
}

/// Run every embedded PostgreSQL migration on an existing pool.
pub async fn migrate_postgres(pool: &PgPool) -> Result<MigrationOutcome, MigrationRunnerError> {
    migrate_postgres_with_flag(pool, false).await
}

/// Run migrations unless automatic migration has been disabled.
pub async fn migrate_postgres_with_flag(
    pool: &PgPool,
    disable_auto_migration: bool,
) -> Result<MigrationOutcome, MigrationRunnerError> {
    if disable_auto_migration {
        verify_postgres_schema_current(pool).await?;
        return Ok(MigrationOutcome::Disabled);
    }

    // Serialize the complete catalog across application instances and keep a
    // failed migration atomic.
    const MIGRATION_LOCK_KEY: i64 = 0x4158_4F4E_4855_4221; // "CONDUIT!"
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *transaction)
        .await?;
    let outcome = run_migrations_postgres(&mut transaction, false).await?;
    transaction.commit().await?;
    Ok(outcome)
}

async fn verify_postgres_schema_current(pool: &PgPool) -> Result<(), MigrationRunnerError> {
    let tracking_table_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
         WHERE table_schema=current_schema() AND table_name='schema_migrations')",
    )
    .fetch_one(pool)
    .await?;
    if !tracking_table_exists {
        return Err(MigrationRunnerError::SchemaVersion(format!(
            "automatic migration is disabled, but schema_migrations is missing; expected {LATEST_SCHEMA_VERSION}"
        )));
    }

    let applied = sqlx::query_scalar::<_, String>("SELECT version FROM schema_migrations")
        .fetch_all(pool)
        .await?;
    let latest = applied.iter().max().map(String::as_str);
    let missing = required_postgres_schema_versions()
        .filter(|required| !applied.iter().any(|version| version == required))
        .collect::<Vec<_>>();
    if latest != Some(LATEST_SCHEMA_VERSION) || !missing.is_empty() {
        return Err(MigrationRunnerError::SchemaVersion(format!(
            "automatic migration is disabled, but expected latest {LATEST_SCHEMA_VERSION} with all embedded migrations applied; latest recorded is {}, missing [{}]",
            latest.unwrap_or("none"),
            missing.join(", ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::DbDialect;

    fn assert_postgres_error_code(error: &sqlx::Error, expected: &str) {
        assert_eq!(
            error
                .as_database_error()
                .and_then(|database_error| database_error.code())
                .as_deref(),
            Some(expected),
            "unexpected PostgreSQL error: {error}"
        );
    }

    fn require_postgres_error<T>(
        result: Result<T, sqlx::Error>,
        context: &'static str,
    ) -> Result<sqlx::Error, Box<dyn std::error::Error>> {
        match result {
            Ok(_) => Err(context.into()),
            Err(error) => Ok(error),
        }
    }

    #[tokio::test]
    async fn connect_then_migrate_when_test_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let timezone: String = sqlx::query_scalar("SHOW TIME ZONE")
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(timezone, "UTC");
        let latest: String = sqlx::query_scalar(
            "SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1",
        )
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(latest, crate::migrate::LATEST_SCHEMA_VERSION);
        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn disabled_auto_migration_requires_the_complete_current_schema_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        assert_eq!(
            migrate_postgres_with_flag(&database.pool, true).await?,
            MigrationOutcome::Disabled
        );

        sqlx::query("DELETE FROM schema_migrations WHERE version=$1")
            .bind(crate::migrate::PRICING_CHANGE_AUDITS_SCHEMA_VERSION)
            .execute(&database.pool)
            .await?;
        let error = match migrate_postgres_with_flag(&database.pool, true).await {
            Ok(outcome) => {
                return Err(format!(
                    "a schema with a missing embedded migration was accepted as {outcome:?}"
                )
                .into());
            }
            Err(error) => error,
        };
        let MigrationRunnerError::SchemaVersion(message) = error else {
            panic!("unexpected migration error: {error}");
        };
        assert!(message.contains(crate::migrate::PRICING_CHANGE_AUDITS_SCHEMA_VERSION));
        assert!(message.contains(crate::migrate::LATEST_SCHEMA_VERSION));

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn fresh_migrations_enforce_subscription_billing_relations()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;

        let user_a = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,password) VALUES('billing-a@example.test','unused') RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let user_b = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users(email,password) VALUES('billing-b@example.test','unused') RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let plan_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO subscription_plans(name,interval_unit,created_at,updated_at) \
             VALUES('billing-integrity','month',now(),now()) RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;

        let invalid_user = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES(9223372036854775807,$1,'invalid-user','{}'::jsonb, \
                    '2026-01-01','2026-02-01',now(),now())",
            )
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "unknown users must not own subscriptions",
        )?;
        assert_postgres_error_code(&invalid_user, "23503");

        let invalid_plan = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,9223372036854775807,'invalid-plan','{}'::jsonb, \
                    '2026-01-01','2026-02-01',now(),now())",
            )
            .bind(user_a)
            .execute(&database.pool)
            .await,
            "subscriptions must reference an existing plan",
        )?;
        assert_postgres_error_code(&invalid_plan, "23503");

        let invalid_period = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,$2,'invalid-period','{}'::jsonb, \
                    '2026-02-01','2026-02-01',now(),now())",
            )
            .bind(user_a)
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "subscription periods must have positive duration",
        )?;
        assert_postgres_error_code(&invalid_period, "23514");

        let invalid_interval = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot,current_period_start, \
              current_period_end,assigned_interval_count,created_at,updated_at) \
             VALUES($1,$2,'invalid-interval','{}'::jsonb, \
                    '2026-01-01','2026-02-01',0,now(),now())",
            )
            .bind(user_a)
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "assigned intervals must be positive",
        )?;
        assert_postgres_error_code(&invalid_interval, "23514");

        let empty_assignment_key = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,$2,'','{}'::jsonb,'2026-01-01','2026-02-01',now(),now())",
            )
            .bind(user_a)
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "assignment keys must not be empty",
        )?;
        assert_postgres_error_code(&empty_assignment_key, "23514");

        let padded_assignment_key = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,$2,' padded-assignment ','{}'::jsonb, \
                    '2026-01-01','2026-02-01',now(),now())",
            )
            .bind(user_a)
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "assignment keys must not have surrounding whitespace",
        )?;
        assert_postgres_error_code(&padded_assignment_key, "23514");

        let non_object_assignment_snapshot = require_postgres_error(
            sqlx::query(
                "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,$2,'invalid-snapshot','[]'::jsonb, \
                    '2026-01-01','2026-02-01',now(),now())",
            )
            .bind(user_a)
            .bind(plan_id)
            .execute(&database.pool)
            .await,
            "assignment request snapshots must be JSON objects",
        )?;
        assert_postgres_error_code(&non_object_assignment_snapshot, "23514");

        let create_subscription = "INSERT INTO user_subscriptions \
             (user_id,plan_id,assignment_key,assignment_request_snapshot, \
              current_period_start,current_period_end,created_at,updated_at) \
             VALUES($1,$2,$3,'{}'::jsonb,'2026-01-01','2026-02-01',now(),now()) RETURNING id";
        let subscription_a = sqlx::query_scalar::<_, i64>(create_subscription)
            .bind(user_a)
            .bind(plan_id)
            .bind("billing-assignment-a")
            .fetch_one(&database.pool)
            .await?;
        let subscription_b = sqlx::query_scalar::<_, i64>(create_subscription)
            .bind(user_b)
            .bind(plan_id)
            .bind("billing-assignment-b")
            .fetch_one(&database.pool)
            .await?;

        let duplicate_assignment_key = require_postgres_error(
            sqlx::query_scalar::<_, i64>(create_subscription)
                .bind(user_b)
                .bind(plan_id)
                .bind("billing-assignment-a")
                .fetch_one(&database.pool)
                .await,
            "assignment keys must be globally unique",
        )?;
        assert_postgres_error_code(&duplicate_assignment_key, "23505");

        let rule_a = sqlx::query_scalar::<_, i64>(
            "INSERT INTO user_subscription_quota_rule_snapshots \
             (subscription_id,rule_key,rule_name,quota_class,amount_micros,rollover_mode,created_at) \
             VALUES($1,'general','General','GENERAL',1000000,'none',now()) RETURNING id",
        )
        .bind(subscription_a)
        .fetch_one(&database.pool)
        .await?;
        let create_entitlement = "INSERT INTO subscription_entitlement_snapshots \
             (subscription_id,period_start,period_end,created_at) \
             VALUES($1,'2026-01-01','2026-02-01',now()) RETURNING id";
        let entitlement_a = sqlx::query_scalar::<_, i64>(create_entitlement)
            .bind(subscription_a)
            .fetch_one(&database.pool)
            .await?;
        let entitlement_b = sqlx::query_scalar::<_, i64>(create_entitlement)
            .bind(subscription_b)
            .fetch_one(&database.pool)
            .await?;

        let insert_bucket = "INSERT INTO subscription_allowance_buckets \
             (subscription_id,quota_rule_snapshot_id,entitlement_snapshot_id,quota_class, \
              issued_at,period_start,period_end,expires_at,granted_micros,created_at,updated_at) \
             VALUES($1,$2,$3,$4,now(),'2026-01-01','2026-02-01','2026-02-01',1000000,now(),now())";
        let wrong_rule_owner = require_postgres_error(
            sqlx::query(insert_bucket)
                .bind(subscription_b)
                .bind(rule_a)
                .bind(entitlement_b)
                .bind("GENERAL")
                .execute(&database.pool)
                .await,
            "a bucket cannot borrow another subscription's quota rule snapshot",
        )?;
        assert_postgres_error_code(&wrong_rule_owner, "23503");

        let wrong_entitlement_owner = require_postgres_error(
            sqlx::query(insert_bucket)
                .bind(subscription_a)
                .bind(rule_a)
                .bind(entitlement_b)
                .bind("GENERAL")
                .execute(&database.pool)
                .await,
            "a bucket cannot borrow another subscription's entitlement snapshot",
        )?;
        assert_postgres_error_code(&wrong_entitlement_owner, "23503");

        let wrong_quota_class = require_postgres_error(
            sqlx::query(insert_bucket)
                .bind(subscription_a)
                .bind(rule_a)
                .bind(entitlement_a)
                .bind("DEDICATED")
                .execute(&database.pool)
                .await,
            "a bucket's quota class must match its rule snapshot",
        )?;
        assert_postgres_error_code(&wrong_quota_class, "23503");

        sqlx::query(insert_bucket)
            .bind(subscription_a)
            .bind(rule_a)
            .bind(entitlement_a)
            .bind("GENERAL")
            .execute(&database.pool)
            .await?;
        sqlx::query(
            "INSERT INTO subscription_entitlement_snapshot_items \
             (snapshot_id,quota_rule_snapshot_id,public_model_id) VALUES($1,$2,1)",
        )
        .bind(entitlement_a)
        .bind(rule_a)
        .execute(&database.pool)
        .await?;

        sqlx::query("DELETE FROM user_subscriptions WHERE id=$1")
            .bind(subscription_a)
            .execute(&database.pool)
            .await?;
        let remaining_children = sqlx::query_scalar::<_, i64>(
            "SELECT \
               (SELECT count(*) FROM user_subscription_quota_rule_snapshots WHERE subscription_id=$1) + \
               (SELECT count(*) FROM subscription_entitlement_snapshots WHERE subscription_id=$1) + \
               (SELECT count(*) FROM subscription_allowance_buckets WHERE subscription_id=$1)",
        )
        .bind(subscription_a)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(remaining_children, 0);

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn pool_set_can_fallback_when_replica_is_unreachable()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let mut config = DatabaseConfig::new(DbDialect::Postgres, dsn);
        config.max_connections = 1;
        config.min_connections = 0;
        let pools = connect_postgres_pools(
            &config,
            Some("postgres://127.0.0.1:1/unreachable"),
            1,
            0,
            true,
        )
        .await?;
        assert!(pools.read().is_none());
        assert!(pools.fallback_on_replica_failure());
        pools.master().close().await;
        Ok(())
    }
}

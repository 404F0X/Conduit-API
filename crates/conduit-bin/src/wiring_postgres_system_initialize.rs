//! Atomic PostgreSQL implementation of the first-run bootstrap.
//!
//! The domain service also has repository-by-repository bootstrap support for
//! in-memory tests. Production must not use that path: independent pool calls
//! cannot provide an all-or-nothing installation and a failed tail write can
//! strand unique owner/project/storage rows. This module deliberately owns the
//! PostgreSQL transaction boundary and serializes competing installers with a
//! transaction-scoped advisory lock.

use conduit_services::{InitializeParams, bootstrap_general_settings_value, system_key};
use sqlx::{PgPool, Postgres, Transaction, types::Json};

const INITIALIZE_LOCK_KEY: i64 = 0x434f_4e44_5549_5401;

/// Number of writes made by a bootstrap with a non-empty version. Kept for the
/// fault-injection regression so every mutation boundary is exercised.
#[cfg(test)]
const BOOTSTRAP_WRITE_COUNT: usize = 13;

pub(crate) async fn initialize_system(
    pool: &PgPool,
    params: &InitializeParams,
) -> Result<(), String> {
    initialize_system_with_failure(pool, params, None).await
}

async fn initialize_system_with_failure(
    pool: &PgPool,
    params: &InitializeParams,
    fail_after_write: Option<usize>,
) -> Result<(), String> {
    let general_settings = bootstrap_general_settings_value(&params.accounting_settings)
        .map_err(|error| format!("invalid bootstrap general settings: {error}"))?;
    let general_settings = serde_json::to_string(&general_settings)
        .map_err(|error| format!("failed to encode bootstrap general settings: {error}"))?;

    // Hash before opening the transaction so the intentionally expensive
    // bcrypt work neither occupies a connection nor extends the advisory lock.
    let password_hash = conduit_auth::encode_password_bcrypt_hex(
        &params.owner_password,
        conduit_auth::DEFAULT_BCRYPT_COST,
    )
    .map_err(|error| format!("failed to hash owner password: {error}"))?;
    let jwt_secret = conduit_auth::generate_secret_key();

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| db_error("begin bootstrap transaction", error))?;
    let result = initialize_in_transaction(
        &mut tx,
        params,
        &password_hash,
        &jwt_secret,
        &general_settings,
        fail_after_write,
    )
    .await;

    match result {
        Ok(()) => tx
            .commit()
            .await
            .map_err(|error| db_error("commit bootstrap transaction", error)),
        Err(error) => {
            if let Err(rollback_error) = tx.rollback().await {
                return Err(format!(
                    "{error}; bootstrap rollback failed: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}

async fn initialize_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    params: &InitializeParams,
    password_hash: &str,
    jwt_secret: &str,
    general_settings: &str,
    fail_after_write: Option<usize>,
) -> Result<(), String> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(INITIALIZE_LOCK_KEY)
        .execute(&mut **tx)
        .await
        .map_err(|error| db_error("lock system initialization", error))?;

    // This check must happen after the lock is held. Otherwise two fresh
    // requests can both observe `false` and the loser reports an unrelated
    // unique-constraint error instead of the stable idempotency result.
    let initialized = sqlx::query_scalar::<_, String>(
        "SELECT value FROM systems WHERE key = $1 AND deleted_at = 0",
    )
    .bind(system_key::INITIALIZED)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| db_error("read initialization state", error))?;
    if initialized
        .as_deref()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
    {
        return Err("system is already initialized".to_string());
    }

    let mut write_number = 0_usize;
    let prefer_language = params
        .prefer_language
        .as_deref()
        .filter(|language| !language.is_empty())
        .unwrap_or("en");
    let owner_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO users \
         (email,status,prefer_language,password,first_name,last_name,avatar,is_owner,scopes) \
         VALUES ($1,'activated',$2,$3,$4,$5,NULL,TRUE,$6) RETURNING id",
    )
    .bind(&params.owner_email)
    .bind(prefer_language)
    .bind(password_hash)
    .bind(params.owner_first_name.as_deref().unwrap_or_default())
    .bind(params.owner_last_name.as_deref().unwrap_or_default())
    .bind(Json(vec!["*".to_string()]))
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| db_error("create bootstrap owner", error))?;
    checkpoint(&mut write_number, fail_after_write)?;

    let project_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO projects (name,status,description,profiles) \
         VALUES ('Default','active','Default project','{}'::jsonb) RETURNING id",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| db_error("create default project", error))?;
    checkpoint(&mut write_number, fail_after_write)?;

    for (name, scopes) in default_project_roles() {
        sqlx::query(
            "INSERT INTO roles (name,level,project_id,scopes) \
             VALUES ($1,'project',$2,$3)",
        )
        .bind(name)
        .bind(project_id)
        .bind(Json(scopes))
        .execute(&mut **tx)
        .await
        .map_err(|error| db_error("create default project role", error))?;
        checkpoint(&mut write_number, fail_after_write)?;
    }

    sqlx::query(
        "INSERT INTO user_projects (user_id,project_id,is_owner,scopes) \
         VALUES ($1,$2,TRUE,'[]'::jsonb)",
    )
    .bind(owner_id)
    .bind(project_id)
    .execute(&mut **tx)
    .await
    .map_err(|error| db_error("assign bootstrap owner to project", error))?;
    checkpoint(&mut write_number, fail_after_write)?;

    let storage_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO data_storages \
         (name,description,\"primary\",\"type\",settings,status) \
         VALUES ('Primary','Primary database storage',TRUE,'database','{}'::jsonb,'active') \
         RETURNING id",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| db_error("create primary data storage", error))?;
    checkpoint(&mut write_number, fail_after_write)?;

    write_system_value(tx, system_key::JWT_SECRET_KEY, jwt_secret).await?;
    checkpoint(&mut write_number, fail_after_write)?;
    write_system_value(tx, system_key::BRAND_NAME, &params.brand_name).await?;
    checkpoint(&mut write_number, fail_after_write)?;
    write_system_value(
        tx,
        system_key::DEFAULT_DATA_STORAGE_ID,
        &storage_id.to_string(),
    )
    .await?;
    checkpoint(&mut write_number, fail_after_write)?;

    if !params.version.is_empty() {
        write_system_value(tx, system_key::VERSION, &params.version).await?;
        checkpoint(&mut write_number, fail_after_write)?;
    }

    write_system_value(tx, system_key::GENERAL_SETTINGS, general_settings).await?;
    checkpoint(&mut write_number, fail_after_write)?;

    // The initialized flag is the final mutation in the same transaction.
    write_system_value(tx, system_key::INITIALIZED, "true").await?;
    checkpoint(&mut write_number, fail_after_write)?;
    Ok(())
}

async fn write_system_value(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO systems (key,value,created_at,updated_at,deleted_at) \
         VALUES ($1,$2,now(),now(),0) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=now(),deleted_at=0",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await
    .map_err(|error| db_error("write bootstrap system value", error))?;
    Ok(())
}

fn checkpoint(write_number: &mut usize, fail_after_write: Option<usize>) -> Result<(), String> {
    *write_number += 1;
    if fail_after_write == Some(*write_number) {
        return Err(format!(
            "injected bootstrap failure after write {}",
            *write_number
        ));
    }
    Ok(())
}

fn default_project_roles() -> [(&'static str, Vec<String>); 3] {
    [
        (
            "Admin",
            [
                "read_users",
                "write_users",
                "read_roles",
                "write_roles",
                "read_api_keys",
                "write_api_keys",
                "read_requests",
                "write_requests",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        ),
        (
            "Developer",
            [
                "read_users",
                "read_api_keys",
                "write_api_keys",
                "read_requests",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        ),
        (
            "Viewer",
            ["read_users", "read_requests"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    ]
}

fn db_error(context: &str, error: sqlx::Error) -> String {
    format!("{context} failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(email: &str) -> InitializeParams {
        InitializeParams {
            owner_email: email.to_string(),
            owner_password: "test-password-never-logged".to_string(),
            owner_first_name: Some("Test".to_string()),
            owner_last_name: Some("Owner".to_string()),
            brand_name: "Conduit API".to_string(),
            prefer_language: Some("en".to_string()),
            accounting_settings: conduit_core::objects::money::AccountingSettings {
                accounting_currency: "USD".to_string(),
                credit_display_name: "API credits".to_string(),
                credits_per_accounting_unit: rust_decimal::Decimal::from(2_500),
                exchange_rates: Vec::new(),
                version: 1,
            },
            version: "0.1.0-test".to_string(),
            now: chrono::Utc::now().to_rfc3339(),
        }
    }

    async fn count(pool: &PgPool, table: &str) -> Result<i64, sqlx::Error> {
        // Callers only pass the fixed table names below; identifiers cannot be
        // bound by PostgreSQL, so keep the interpolation inside this test helper.
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(pool)
            .await
    }

    async fn assert_empty(pool: &PgPool) -> Result<(), Box<dyn std::error::Error>> {
        for table in [
            "user_projects",
            "roles",
            "projects",
            "users",
            "data_storages",
            "systems",
        ] {
            assert_eq!(
                count(pool, table).await?,
                0,
                "table {table} was not rolled back"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn every_bootstrap_write_rolls_back_and_retry_succeeds()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let init = params("atomic-bootstrap@example.test");

        for write in 1..=BOOTSTRAP_WRITE_COUNT {
            let error = initialize_system_with_failure(&database.pool, &init, Some(write))
                .await
                .expect_err("the selected write boundary must fail");
            assert!(error.contains("injected bootstrap failure"));
            assert_empty(&database.pool).await?;
        }

        initialize_system(&database.pool, &init).await?;
        assert_eq!(count(&database.pool, "users").await?, 1);
        assert_eq!(count(&database.pool, "projects").await?, 1);
        assert_eq!(count(&database.pool, "roles").await?, 3);
        assert_eq!(count(&database.pool, "user_projects").await?, 1);
        assert_eq!(count(&database.pool, "data_storages").await?, 1);
        assert_eq!(count(&database.pool, "systems").await?, 6);
        let stored = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key = $1 AND deleted_at = 0",
        )
        .bind(system_key::GENERAL_SETTINGS)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stored)?,
            bootstrap_general_settings_value(&init.accounting_settings)
                .expect("test accounting settings are valid")
        );

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn advisory_lock_allows_only_one_concurrent_initializer()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let left_pool = database.pool.clone();
        let right_pool = database.pool.clone();
        let left = tokio::spawn(async move {
            initialize_system(&left_pool, &params("left-bootstrap@example.test")).await
        });
        let right = tokio::spawn(async move {
            initialize_system(&right_pool, &params("right-bootstrap@example.test")).await
        });
        let outcomes = [left.await?, right.await?];

        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        let error = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .expect("one concurrent initializer must lose");
        assert_eq!(error, "system is already initialized");
        assert_eq!(count(&database.pool, "users").await?, 1);

        database.cleanup().await?;
        Ok(())
    }
}

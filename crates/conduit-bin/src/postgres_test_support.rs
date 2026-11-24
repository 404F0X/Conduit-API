//! Isolated PostgreSQL schema support for opt-in integration tests.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub(crate) struct IsolatedPostgres {
    pub(crate) pool: PgPool,
    admin_pool: PgPool,
    schema: String,
    private_database: Option<String>,
}

impl IsolatedPostgres {
    pub(crate) async fn owners(&self, ids: &[i64]) -> Result<(), sqlx::Error> {
        for id in ids {
            sqlx::query("INSERT INTO users(id,email,password,status,is_owner) VALUES($1,$2,'fixture','activated',true)")
                .bind(id)
                .bind(format!("owner-{id}@fixture.invalid"))
                .execute(&self.pool)
                .await?;
        }
        sqlx::query(
            "SELECT setval(pg_get_serial_sequence('users','id'), (SELECT MAX(id) FROM users))",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let admin_pool = PgPool::connect(dsn).await?;
        let schema = format!("conduit_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!(
            r#"CREATE SCHEMA "{schema}""#
        )))
        .execute(&admin_pool)
        .await?;
        let search_path = format!(r#"SET search_path TO "{schema}""#);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe::<&str>(&search_path))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(dsn)
            .await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        Ok(Self {
            pool,
            admin_pool,
            schema,
            private_database: None,
        })
    }

    /// Database-wide advisory locks must not interact with parallel schema fixtures.
    pub(crate) async fn new_private_database(
        dsn: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let admin_pool = PgPool::connect(dsn).await?;
        let database = format!("conduit_lock_test_{}", uuid::Uuid::new_v4().simple());
        // The identifier consists only of a fixed prefix and UUID hex digits.
        sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!(
            r#"CREATE DATABASE "{database}" TEMPLATE template0"#
        )))
        .execute(&admin_pool)
        .await
        .map_err(|error| {
            format!("private lock-test database creation requires CREATEDB: {error}")
        })?;
        let options = admin_pool
            .connect_options()
            .as_ref()
            .clone()
            .database(&database);
        let connected = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await;
        let pool = match connected {
            Ok(pool) => pool,
            Err(error) => {
                drop_private_database(&admin_pool, &database).await?;
                admin_pool.close().await;
                return Err(error.into());
            }
        };
        let db = Self {
            pool,
            admin_pool,
            schema: String::new(),
            private_database: Some(database),
        };
        if let Err(error) =
            conduit_db::connection::migrate_postgres_with_flag(&db.pool, false).await
        {
            db.cleanup().await?;
            return Err(error.into());
        }
        Ok(db)
    }

    pub(crate) async fn cleanup(self) -> Result<(), sqlx::Error> {
        self.pool.close().await;
        if let Some(database) = &self.private_database {
            let result = drop_private_database(&self.admin_pool, database).await;
            self.admin_pool.close().await;
            return result;
        }
        sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!(
            r#"DROP SCHEMA "{}" CASCADE"#,
            self.schema
        )))
        .execute(&self.admin_pool)
        .await?;
        self.admin_pool.close().await;
        Ok(())
    }
}

async fn drop_private_database(admin: &PgPool, database: &str) -> Result<(), sqlx::Error> {
    let result = sqlx::query(sqlx::AssertSqlSafe::<&str>(&format!(
        r#"DROP DATABASE "{database}" WITH (FORCE)"#
    )))
    .execute(admin)
    .await;
    if let Err(error) = &result {
        eprintln!("failed to clean up owned lock-test database {database}: {error}");
    }
    result.map(|_| ())
}

/// Clean up the owned database on success, returned errors, and assertion panics.
pub(crate) async fn run_private_database<T, F, Fut>(
    dsn: &str,
    body: F,
) -> Result<T, Box<dyn std::error::Error>>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error>>>,
{
    let db = IsolatedPostgres::new_private_database(dsn).await?;
    let pool = db.pool.clone();
    let mut future = Box::pin(async move { body(pool).await });
    let outcome = std::future::poll_fn(|cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
            Err(panic) => std::task::Poll::Ready(Err(panic)),
        }
    })
    .await;
    drop(future);
    let cleanup = db.cleanup().await;
    match outcome {
        Ok(result) => {
            let value = result?;
            cleanup?;
            Ok(value)
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[tokio::test]
async fn private_database_isolates_advisory_locks() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
        return Ok(());
    };
    let base = PgPool::connect(&dsn).await?;
    let base_ref = &base;
    let result = run_private_database(&dsn, |pool| async move {
        let base_oid: i64 = sqlx::query_scalar(
            "SELECT oid::bigint FROM pg_database WHERE datname=current_database()",
        )
        .fetch_one(base_ref)
        .await?;
        let private_oid: i64 = sqlx::query_scalar(
            "SELECT oid::bigint FROM pg_database WHERE datname=current_database()",
        )
        .fetch_one(&pool)
        .await?;
        assert_ne!(base_oid, private_oid);
        let mut base_tx = base_ref.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(786222001)")
            .execute(&mut *base_tx)
            .await?;
        let mut private_tx = pool.begin().await?;
        let across_databases: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(786222001)")
                .fetch_one(&mut *private_tx)
                .await?;
        assert!(
            across_databases,
            "base database must not block private database"
        );
        let mut other_private_tx = pool.begin().await?;
        let within_database: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(786222001)")
                .fetch_one(&mut *other_private_tx)
                .await?;
        assert!(
            !within_database,
            "private database connections must still contend"
        );
        other_private_tx.rollback().await?;
        private_tx.rollback().await?;
        base_tx.rollback().await?;
        Ok(())
    })
    .await;
    base.close().await;
    result
}

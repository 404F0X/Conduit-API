//! Per-test PostgreSQL schema isolation for opt-in integration tests.

use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

static NEXT_SCHEMA: AtomicU64 = AtomicU64::new(1);

pub(crate) struct IsolatedPostgres {
    pub(crate) pool: PgPool,
    admin_pool: PgPool,
    schema: String,
}

impl IsolatedPostgres {
    pub(crate) async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let admin_pool = PgPool::connect(dsn).await?;
        let schema = format!(
            "conduit_db_test_{}_{}",
            std::process::id(),
            NEXT_SCHEMA.fetch_add(1, Ordering::Relaxed)
        );
        sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin_pool)
            .await?;

        let search_path = format!("SET search_path TO \"{schema}\"");
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(dsn)
            .await?;
        crate::connection::migrate_postgres_with_flag(&pool, false).await?;

        Ok(Self {
            pool,
            admin_pool,
            schema,
        })
    }

    pub(crate) async fn cleanup(self) -> Result<(), sqlx::Error> {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
            .execute(&self.admin_pool)
            .await?;
        self.admin_pool.close().await;
        Ok(())
    }
}

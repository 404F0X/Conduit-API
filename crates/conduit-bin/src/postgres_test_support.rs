//! Isolated PostgreSQL schema support for opt-in integration tests.

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub(crate) struct IsolatedPostgres {
    pub(crate) pool: PgPool,
    admin_pool: PgPool,
    schema: String,
}

impl IsolatedPostgres {
    pub(crate) async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let admin_pool = PgPool::connect(dsn).await?;
        let schema = format!("conduit_test_{}", uuid::Uuid::new_v4().simple());
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
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
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

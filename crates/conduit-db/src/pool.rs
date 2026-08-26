use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

/// Default for `db.max_open_conns`.
pub const DEFAULT_MAX_OPEN_CONNS: u32 = 20;
/// Default for `db.max_idle_conns`.
pub const DEFAULT_MAX_IDLE_CONNS: u32 = 10;
/// Default for `db.conn_max_lifetime`.
pub const DEFAULT_CONN_MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);
/// Default for `db.conn_max_idle_time`.
pub const DEFAULT_CONN_MAX_IDLE_TIME: Duration = Duration::from_secs(10 * 60);

/// Runtime database dialect.
///
/// Conduit API's Rust runtime supports PostgreSQL only. The enum is retained so
/// existing configuration and routing call sites can migrate without a second
/// simultaneous public-API change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbDialect {
    Postgres,
}

impl DbDialect {
    pub const fn as_str(self) -> &'static str {
        "postgres"
    }
}

impl Display for DbDialect {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DbDialect {
    type Err = RouterError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" | "pgx" | "postgresdb" => Ok(Self::Postgres),
            other => Err(RouterError::InvalidConfig(format!(
                "unsupported database dialect {other:?}; PostgreSQL is required"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbRole {
    Read,
    Write,
    Transaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlStatementKind {
    Read,
    NonRead,
}

/// PostgreSQL pool/connection configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub dialect: DbDialect,
    pub dsn: String,
    pub read_replicas: Vec<ReplicaConfig>,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout: Duration,
    pub conn_max_lifetime: Duration,
    pub conn_max_idle_time: Duration,
}

impl DatabaseConfig {
    pub fn new(dialect: DbDialect, dsn: impl Into<String>) -> Self {
        Self {
            dialect,
            dsn: dsn.into(),
            read_replicas: Vec::new(),
            max_connections: DEFAULT_MAX_OPEN_CONNS,
            min_connections: DEFAULT_MAX_IDLE_CONNS,
            connect_timeout: Duration::from_secs(30),
            conn_max_lifetime: DEFAULT_CONN_MAX_LIFETIME,
            conn_max_idle_time: DEFAULT_CONN_MAX_IDLE_TIME,
        }
    }

    pub fn master_dsn(&self) -> String {
        self.dsn.clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaConfig {
    pub name: String,
    pub dsn: String,
    pub enabled: bool,
    pub weight: u32,
    /// When true, a runtime failure on this replica falls back to master.
    pub fallback_on_replica_failure: bool,
}

impl ReplicaConfig {
    pub fn new(name: impl Into<String>, dsn: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            dsn: dsn.into(),
            enabled: true,
            weight: 1,
            fallback_on_replica_failure: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolTarget {
    pub name: String,
    pub role: DbRole,
    pub dialect: DbDialect,
    pub dsn: String,
}

impl PoolTarget {
    fn master(config: &DatabaseConfig) -> Self {
        Self {
            name: "master".to_string(),
            role: DbRole::Write,
            dialect: config.dialect,
            dsn: config.master_dsn(),
        }
    }

    fn replica(config: &DatabaseConfig, replica: &ReplicaConfig) -> Self {
        Self {
            name: replica.name.clone(),
            role: DbRole::Read,
            dialect: config.dialect,
            dsn: replica.dsn.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PoolRouter {
    master: PoolTarget,
    replicas: Vec<PoolTarget>,
    fallback_on_replica_failure: bool,
}

impl PoolRouter {
    pub fn new(config: DatabaseConfig) -> Result<Self, RouterError> {
        if config.dsn.trim().is_empty() {
            return Err(RouterError::InvalidConfig(
                "database master dsn must not be empty".to_string(),
            ));
        }

        let fallback_on_replica_failure = config
            .read_replicas
            .iter()
            .find(|replica| replica.enabled && replica.weight > 0)
            .is_some_and(|replica| replica.fallback_on_replica_failure);
        let replicas = config
            .read_replicas
            .iter()
            .filter(|replica| replica.enabled && replica.weight > 0)
            .map(|replica| PoolTarget::replica(&config, replica))
            .collect();

        Ok(Self {
            master: PoolTarget::master(&config),
            replicas,
            fallback_on_replica_failure,
        })
    }

    pub const fn master(&self) -> &PoolTarget {
        &self.master
    }

    pub fn replicas(&self) -> &[PoolTarget] {
        &self.replicas
    }

    pub const fn fallback_on_replica_failure(&self) -> bool {
        self.fallback_on_replica_failure
    }

    pub fn read(&self) -> &PoolTarget {
        self.replicas.first().unwrap_or(&self.master)
    }

    pub const fn write(&self) -> &PoolTarget {
        &self.master
    }

    pub const fn transaction(&self) -> &PoolTarget {
        &self.master
    }

    pub fn read_route(&self) -> ReadRoute<'_> {
        match self.replicas.first() {
            Some(target) => ReadRoute::Replica {
                target,
                fallback_to_master: self.fallback_on_replica_failure,
            },
            None => ReadRoute::Master(&self.master),
        }
    }

    pub fn route(&self, role: DbRole) -> &PoolTarget {
        match role {
            DbRole::Read => self.read(),
            DbRole::Write => self.write(),
            DbRole::Transaction => self.transaction(),
        }
    }

    pub fn route_sql(&self, sql: &str) -> &PoolTarget {
        match classify_sql(sql) {
            SqlStatementKind::Read => self.read(),
            SqlStatementKind::NonRead => self.write(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRoute<'a> {
    Replica {
        target: &'a PoolTarget,
        fallback_to_master: bool,
    },
    Master(&'a PoolTarget),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RouterError {
    #[error("invalid database config: {0}")]
    InvalidConfig(String),
}

/// Placeholder retained for callers that have not yet moved to a typed
/// `PostgresPools` handle.
#[derive(Debug, Clone, Copy)]
pub struct DbPoolHandle {
    _private: (),
}

impl DbPoolHandle {
    pub const fn pending_sqlx_any_pool() -> Self {
        Self { _private: () }
    }
}

pub fn classify_sql(sql: &str) -> SqlStatementKind {
    match first_sql_token(sql) {
        Some(token)
            if token.eq_ignore_ascii_case("select") || token.eq_ignore_ascii_case("with") =>
        {
            SqlStatementKind::Read
        }
        _ => SqlStatementKind::NonRead,
    }
}

fn first_sql_token(sql: &str) -> Option<&str> {
    let mut offset = 0;
    while offset < sql.len() {
        let remaining = &sql[offset..];
        let trimmed = remaining.trim_start();
        offset += remaining.len() - trimmed.len();

        if sql[offset..].starts_with("--") {
            let comment = &sql[offset + 2..];
            offset += 2 + comment.find('\n').map_or(comment.len(), |index| index + 1);
            continue;
        }

        if sql[offset..].starts_with("/*") {
            let comment = &sql[offset + 2..];
            offset += 2 + comment.find("*/").map_or(comment.len(), |index| index + 2);
            continue;
        }

        let token_end = sql[offset..]
            .find(|character: char| !character.is_ascii_alphabetic())
            .unwrap_or(sql.len() - offset);
        return (token_end > 0).then_some(&sql[offset..offset + token_end]);
    }

    None
}

/// sqlx `PoolOptions` builder using Conduit API's operator-facing defaults.
pub mod pool_options {
    use std::time::Duration;

    use sqlx::pool::PoolOptions;

    use crate::pool::{
        DEFAULT_CONN_MAX_IDLE_TIME, DEFAULT_CONN_MAX_LIFETIME, DEFAULT_MAX_IDLE_CONNS,
        DEFAULT_MAX_OPEN_CONNS, DatabaseConfig,
    };

    pub fn build_pool_options<DB>(config: &DatabaseConfig) -> PoolOptions<DB>
    where
        DB: sqlx::Database,
    {
        PoolOptions::new()
            .max_connections(if config.max_connections == 0 {
                DEFAULT_MAX_OPEN_CONNS
            } else {
                config.max_connections
            })
            .min_connections(config.min_connections)
            .max_lifetime(nonzero_duration(config.conn_max_lifetime))
            .idle_timeout(nonzero_duration(config.conn_max_idle_time))
            .acquire_timeout(config.connect_timeout)
    }

    fn nonzero_duration(duration: Duration) -> Option<Duration> {
        (duration.as_nanos() > 0).then_some(duration)
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ResolvedPoolOptions {
        pub max_connections: u32,
        pub min_connections: u32,
        pub max_lifetime: Option<Duration>,
        pub idle_timeout: Option<Duration>,
        pub acquire_timeout: Duration,
    }

    pub fn resolved_pool_options(config: &DatabaseConfig) -> ResolvedPoolOptions {
        ResolvedPoolOptions {
            max_connections: if config.max_connections == 0 {
                DEFAULT_MAX_OPEN_CONNS
            } else {
                config.max_connections
            },
            min_connections: config.min_connections,
            max_lifetime: nonzero_duration(config.conn_max_lifetime),
            idle_timeout: nonzero_duration(config.conn_max_idle_time),
            acquire_timeout: config.connect_timeout,
        }
    }

    pub fn default_resolved_pool_options() -> ResolvedPoolOptions {
        ResolvedPoolOptions {
            max_connections: DEFAULT_MAX_OPEN_CONNS,
            min_connections: DEFAULT_MAX_IDLE_CONNS,
            max_lifetime: Some(DEFAULT_CONN_MAX_LIFETIME),
            idle_timeout: Some(DEFAULT_CONN_MAX_IDLE_TIME),
            acquire_timeout: Duration::from_secs(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_postgres_aliases_and_rejects_other_backends() -> Result<(), RouterError> {
        for value in ["postgres", "postgresql", "pg", "pgx", "postgresdb"] {
            assert_eq!(value.parse::<DbDialect>()?, DbDialect::Postgres);
        }
        assert!("sqlite".parse::<DbDialect>().is_err());
        assert!("mysql".parse::<DbDialect>().is_err());
        Ok(())
    }

    #[test]
    fn router_uses_replica_for_reads_and_master_for_writes() -> Result<(), RouterError> {
        let mut config = DatabaseConfig::new(DbDialect::Postgres, "postgres://master");
        config
            .read_replicas
            .push(ReplicaConfig::new("replica-a", "postgres://replica-a"));
        let router = PoolRouter::new(config)?;

        assert_eq!(router.read().name, "replica-a");
        assert_eq!(router.write().name, "master");
        assert_eq!(router.transaction().name, "master");
        assert_eq!(router.route_sql("select 1").name, "replica-a");
        assert_eq!(
            router.route_sql("update users set name = 'a'").name,
            "master"
        );
        Ok(())
    }

    #[test]
    fn router_falls_back_to_master_without_replica() -> Result<(), RouterError> {
        let router = PoolRouter::new(DatabaseConfig::new(
            DbDialect::Postgres,
            "postgres://master",
        ))?;
        assert_eq!(router.read().name, "master");
        assert!(matches!(router.read_route(), ReadRoute::Master(_)));
        Ok(())
    }

    #[test]
    fn fallback_policy_is_carried_by_read_route() -> Result<(), RouterError> {
        let mut config = DatabaseConfig::new(DbDialect::Postgres, "postgres://master");
        config.read_replicas.push(ReplicaConfig {
            fallback_on_replica_failure: true,
            ..ReplicaConfig::new("replica-a", "postgres://replica-a")
        });
        let router = PoolRouter::new(config)?;
        assert!(matches!(
            router.read_route(),
            ReadRoute::Replica {
                fallback_to_master: true,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn classifier_accepts_select_with_and_leading_comments() {
        assert_eq!(classify_sql("select 1"), SqlStatementKind::Read);
        assert_eq!(
            classify_sql("WITH rows AS (SELECT 1) SELECT * FROM rows"),
            SqlStatementKind::Read
        );
        assert_eq!(
            classify_sql(" \n\t-- route hint\n/* block */ SELECT 1"),
            SqlStatementKind::Read
        );
        assert_eq!(classify_sql("/* unterminated"), SqlStatementKind::NonRead);
        assert_eq!(classify_sql(""), SqlStatementKind::NonRead);
    }

    #[test]
    fn pool_option_defaults_and_overrides_are_resolved() {
        use crate::pool::pool_options::{default_resolved_pool_options, resolved_pool_options};

        let config = DatabaseConfig::new(DbDialect::Postgres, "postgres://localhost");
        assert_eq!(
            resolved_pool_options(&config),
            default_resolved_pool_options()
        );

        let mut custom = config;
        custom.max_connections = 50;
        custom.min_connections = 5;
        custom.conn_max_lifetime = Duration::ZERO;
        custom.conn_max_idle_time = Duration::ZERO;
        let resolved = resolved_pool_options(&custom);
        assert_eq!(resolved.max_connections, 50);
        assert_eq!(resolved.min_connections, 5);
        assert_eq!(resolved.max_lifetime, None);
        assert_eq!(resolved.idle_timeout, None);
    }
}

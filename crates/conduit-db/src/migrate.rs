use std::fmt::{Display, Formatter};
use std::str::FromStr;

use thiserror::Error;

/// Runtime migration dialect.
///
/// PostgreSQL is the only supported backend. The enum remains in the public
/// API to avoid coupling backend retirement to an unrelated plan-API rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
}

impl Dialect {
    pub const fn as_str(self) -> &'static str {
        "postgres"
    }
}

impl Display for Dialect {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Dialect {
    type Err = MigrationPlanError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(Self::Postgres),
            other => Err(MigrationPlanError::UnsupportedDialect(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPolicy {
    pub allow_destructive_sql: bool,
    pub require_dialect_match: bool,
}

impl RunnerPolicy {
    pub const fn non_destructive() -> Self {
        Self {
            allow_destructive_sql: false,
            require_dialect_match: true,
        }
    }
}

impl Default for RunnerPolicy {
    fn default() -> Self {
        Self::non_destructive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStep {
    pub id: String,
    pub dialect: Dialect,
    pub sql: String,
}

impl MigrationStep {
    pub fn new(id: impl Into<String>, dialect: Dialect, sql: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            dialect,
            sql: sql.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    pub dialect: Dialect,
    pub steps: Vec<MigrationStep>,
    pub policy: RunnerPolicy,
}

impl MigrationPlan {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            steps: Vec::new(),
            policy: RunnerPolicy::default(),
        }
    }

    pub fn with_policy(mut self, policy: RunnerPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn push_step(&mut self, step: MigrationStep) {
        self.steps.push(step);
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MigrationPlanError {
    #[error("unsupported migration dialect {0:?}; PostgreSQL is required")]
    UnsupportedDialect(String),
    #[error("migration step {step_id:?} uses dialect {step_dialect}, expected {plan_dialect}")]
    DialectMismatch {
        step_id: String,
        plan_dialect: Dialect,
        step_dialect: Dialect,
    },
    #[error("migration step {step_id:?} contains destructive SQL: {reason}")]
    DestructiveSql { step_id: String, reason: String },
}

pub type MigrationPlanResult<T> = Result<T, MigrationPlanError>;

pub const INITIAL_SCHEMA_VERSION: &str = "000001";
pub const COMMERCIALIZATION_SCHEMA_VERSION: &str = "000002";
pub const BALANCE_SUBSCRIPTION_SCHEMA_VERSION: &str = "000003";
pub const SUBSCRIPTION_ENTITLEMENTS_SCHEMA_VERSION: &str = "000004";
pub const PROJECT_COMMERCIALIZATION_SCHEMA_VERSION: &str = "000005";
pub const SUBSCRIPTION_PROJECT_GRANTS_SCHEMA_VERSION: &str = "000006";
pub const PROJECT_WALLET_SHADOW_SCHEMA_VERSION: &str = "000007";
pub const PROJECT_WALLET_SHADOW_LIFECYCLE_SCHEMA_VERSION: &str = "000008";
pub const COMMERCIAL_OPERATION_AUDIT_SCHEMA_VERSION: &str = "000009";
pub const SIMPLE_GROUP_SCHEMA_VERSION: &str = "000011";
pub const SIMPLE_GROUP_MEMBERSHIP_SCHEMA_VERSION: &str = "000012";
pub const CHANNEL_QUOTA_SNAPSHOT_SCHEMA_VERSION: &str = "000015";
pub const PROVIDER_QUOTA_PROBE_VERIFICATION_SCHEMA_VERSION: &str = "000016";
pub const PROVIDER_OBSERVATIONS_SCHEMA_VERSION: &str = "000017";
pub const API_KEY_QUOTA_ADMISSIONS_SCHEMA_VERSION: &str = "000018";
pub const SUBSCRIPTION_ASSIGNMENT_SNAPSHOTS_SCHEMA_VERSION: &str = "000019";
pub const SUBSCRIPTION_PLAN_SNAPSHOTS_SCHEMA_VERSION: &str = "000020";
pub const REQUEST_ROUTE_EXPLANATIONS_SCHEMA_VERSION: &str = "000021";
pub const REQUEST_EXECUTION_CREDENTIAL_IDENTITY_SCHEMA_VERSION: &str = "000022";
pub const POSTGRES_PERFORMANCE_INDEXES_SCHEMA_VERSION: &str = "000023";
pub const POSTGRES_INDEX_CLEANUP_SCHEMA_VERSION: &str = "000024";
pub const POSTGRES_USAGE_INDEX_CLEANUP_SCHEMA_VERSION: &str = "000025";
pub const PROJECT_WALLET_BALANCE_SNAPSHOTS_SCHEMA_VERSION: &str = "000026";
pub const USAGE_CHARGE_OUTBOX_SCHEMA_VERSION: &str = "000027";
pub const ACCOUNTING_CURRENCY_SCHEMA_VERSION: &str = "000028";
pub const ROUTE_AFFINITIES_SCHEMA_VERSION: &str = "000029";
pub const PROVIDER_PRICE_ACCOUNTING_SCHEMA_VERSION: &str = "000030";
pub const PRICING_CHANGE_AUDITS_SCHEMA_VERSION: &str = "000031";
pub const CHANGE_SETS_SCHEMA_VERSION: &str = "000032";
pub const LATEST_SCHEMA_VERSION: &str = CHANGE_SETS_SCHEMA_VERSION;
pub const SCHEMA_MIGRATIONS_TABLE: &str = "schema_migrations";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Disabled,
    AlreadyApplied { version: &'static str },
    Applied { version: &'static str },
}

#[derive(Debug, Error)]
pub enum MigrationRunnerError {
    #[error("migration plan rejected: {0}")]
    PlanRejected(#[from] MigrationPlanError),
    #[error("database schema verification failed: {0}")]
    SchemaVersion(String),
    #[error("database error during migration: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy)]
struct EmbeddedMigration {
    version: &'static str,
    sql: &'static str,
}

const EMBEDDED_MIGRATIONS: &[EmbeddedMigration] = &[
    EmbeddedMigration {
        version: INITIAL_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000001_initial.sql"),
    },
    EmbeddedMigration {
        version: COMMERCIALIZATION_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000002_commercialization_v2.sql"),
    },
    EmbeddedMigration {
        version: BALANCE_SUBSCRIPTION_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000003_balance_subscription_v1.sql"),
    },
    EmbeddedMigration {
        version: SUBSCRIPTION_ENTITLEMENTS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000004_subscription_entitlements.sql"),
    },
    EmbeddedMigration {
        version: PROJECT_COMMERCIALIZATION_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000005_project_commercialization_v3.sql"),
    },
    EmbeddedMigration {
        version: SUBSCRIPTION_PROJECT_GRANTS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000006_subscription_project_grants.sql"),
    },
    EmbeddedMigration {
        version: PROJECT_WALLET_SHADOW_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000007_project_wallet_shadow.sql"),
    },
    EmbeddedMigration {
        version: PROJECT_WALLET_SHADOW_LIFECYCLE_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000008_project_wallet_shadow_lifecycle.sql"
        ),
    },
    EmbeddedMigration {
        version: COMMERCIAL_OPERATION_AUDIT_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000009_commercial_operation_audit.sql"),
    },
    EmbeddedMigration {
        version: SIMPLE_GROUP_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000011_simple_groups.sql"),
    },
    EmbeddedMigration {
        version: SIMPLE_GROUP_MEMBERSHIP_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000012_simple_group_membership.sql"),
    },
    EmbeddedMigration {
        version: CHANNEL_QUOTA_SNAPSHOT_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000015_channel_quota_snapshot.sql"),
    },
    EmbeddedMigration {
        version: PROVIDER_QUOTA_PROBE_VERIFICATION_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000016_provider_quota_probe_verification.sql"
        ),
    },
    EmbeddedMigration {
        version: PROVIDER_OBSERVATIONS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000017_provider_observations.sql"),
    },
    EmbeddedMigration {
        version: API_KEY_QUOTA_ADMISSIONS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000018_api_key_quota_admissions.sql"),
    },
    EmbeddedMigration {
        version: SUBSCRIPTION_ASSIGNMENT_SNAPSHOTS_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000019_subscription_assignment_snapshots.sql"
        ),
    },
    EmbeddedMigration {
        version: SUBSCRIPTION_PLAN_SNAPSHOTS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000020_subscription_plan_snapshots.sql"),
    },
    EmbeddedMigration {
        version: REQUEST_ROUTE_EXPLANATIONS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000021_request_route_explanations.sql"),
    },
    EmbeddedMigration {
        version: REQUEST_EXECUTION_CREDENTIAL_IDENTITY_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000022_request_execution_credential_identity.sql"
        ),
    },
    EmbeddedMigration {
        version: POSTGRES_PERFORMANCE_INDEXES_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000023_postgres_performance_indexes.sql"),
    },
    EmbeddedMigration {
        version: POSTGRES_INDEX_CLEANUP_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000024_remove_redundant_request_index.sql"),
    },
    EmbeddedMigration {
        version: POSTGRES_USAGE_INDEX_CLEANUP_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000025_remove_unhelpful_usage_indexes.sql"),
    },
    EmbeddedMigration {
        version: PROJECT_WALLET_BALANCE_SNAPSHOTS_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000026_project_wallet_balance_snapshots.sql"
        ),
    },
    EmbeddedMigration {
        version: USAGE_CHARGE_OUTBOX_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000027_usage_charge_outbox.sql"),
    },
    EmbeddedMigration {
        version: ACCOUNTING_CURRENCY_SCHEMA_VERSION,
        sql: include_str!(
            "../../../migrations/postgres/000028_accounting_currency_station_credit.sql"
        ),
    },
    EmbeddedMigration {
        version: ROUTE_AFFINITIES_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000029_route_affinities.sql"),
    },
    EmbeddedMigration {
        version: PROVIDER_PRICE_ACCOUNTING_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000030_provider_price_accounting.sql"),
    },
    EmbeddedMigration {
        version: PRICING_CHANGE_AUDITS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000031_pricing_change_audits.sql"),
    },
    EmbeddedMigration {
        version: CHANGE_SETS_SCHEMA_VERSION,
        sql: include_str!("../../../migrations/postgres/000032_change_sets.sql"),
    },
];

pub(crate) fn required_postgres_schema_versions() -> impl Iterator<Item = &'static str> {
    EMBEDDED_MIGRATIONS
        .iter()
        .map(|migration| migration.version)
}

pub fn initial_sql_for_dialect(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::Postgres => Some(EMBEDDED_MIGRATIONS[0].sql),
    }
}

fn migrations_for_dialect(dialect: Dialect) -> Vec<EmbeddedMigration> {
    match dialect {
        Dialect::Postgres => EMBEDDED_MIGRATIONS.to_vec(),
    }
}

/// Compatibility diagnostic for the retained dialect API.
pub fn select_dialect_entrypoint(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Postgres => "run_migrations_postgres",
    }
}

/// Apply every missing embedded PostgreSQL migration in version order.
pub async fn run_migrations_postgres(
    conn: &mut sqlx::PgConnection,
    disable_auto_migration: bool,
) -> Result<MigrationOutcome, MigrationRunnerError> {
    if disable_auto_migration {
        return Ok(MigrationOutcome::Disabled);
    }

    let create_tracking_table = format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
            version TEXT PRIMARY KEY, \
            applied_at TEXT NOT NULL \
         )",
        table = SCHEMA_MIGRATIONS_TABLE,
    );
    sqlx::query(&create_tracking_table)
        .execute(&mut *conn)
        .await?;

    let migrations = migrations_for_dialect(Dialect::Postgres);
    let mut plan = MigrationPlan::new(Dialect::Postgres);
    for migration in &migrations {
        plan.push_step(MigrationStep::new(
            migration.version,
            Dialect::Postgres,
            migration.sql,
        ));
    }
    validate_plan_non_destructive(&plan)?;

    let mut last_applied = None;
    for migration in migrations {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = $1",
        )
        .bind(migration.version)
        .fetch_one(&mut *conn)
        .await?;
        if count > 0 {
            continue;
        }

        for statement in split_ddl_statements(migration.sql) {
            if let Err(error) = sqlx::raw_sql(&statement).execute(&mut *conn).await {
                if is_object_exists_error(&error) {
                    continue;
                }
                return Err(error.into());
            }
        }

        sqlx::query("INSERT INTO schema_migrations (version, applied_at) VALUES ($1, $2)")
            .bind(migration.version)
            .bind(rfc3339_utc_now())
            .execute(&mut *conn)
            .await?;
        last_applied = Some(migration.version);
    }

    Ok(match last_applied {
        Some(version) => MigrationOutcome::Applied { version },
        None => MigrationOutcome::AlreadyApplied {
            version: LATEST_SCHEMA_VERSION,
        },
    })
}

fn is_object_exists_error(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|database_error| database_error.code())
        .is_some_and(|code| matches!(code.as_ref(), "42P07" | "42710" | "42P06" | "42P16"))
}

/// Split a multi-statement PostgreSQL payload while preserving dollar-quoted
/// function bodies.
fn split_ddl_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut dollar_quote: Option<String> = None;

    for line in sql.lines() {
        current.push_str(line);
        current.push('\n');
        let code = match line.find("--") {
            Some(index) => &line[..index],
            None => line,
        };
        let mut offset = 0;
        while offset < code.len() {
            if let Some(delimiter) = dollar_quote.as_deref() {
                let Some(relative) = code[offset..].find(delimiter) else {
                    break;
                };
                offset += relative + delimiter.len();
                dollar_quote = None;
                continue;
            }
            let Some(relative_start) = code[offset..].find('$') else {
                break;
            };
            let start = offset + relative_start;
            let Some(relative_end) = code[start + 1..].find('$') else {
                break;
            };
            let end = start + 1 + relative_end;
            let tag = &code[start + 1..end];
            if tag
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                let delimiter = code[start..=end].to_string();
                offset = end + 1;
                dollar_quote = Some(delimiter);
            } else {
                offset = start + 1;
            }
        }
        if dollar_quote.is_none() && code.trim_end().ends_with(';') {
            let statement = current.trim();
            if !statement.is_empty() {
                statements.push(statement.to_string());
            }
            current.clear();
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        statements.push(tail.to_string());
    }
    statements
}

fn rfc3339_utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = duration.as_secs();
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let hour = (day_seconds / 3_600) as u32;
    let minute = ((day_seconds % 3_600) / 60) as u32;
    let second = (day_seconds % 60) as u32;

    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

pub fn validate_plan_non_destructive(plan: &MigrationPlan) -> MigrationPlanResult<()> {
    for step in &plan.steps {
        if plan.policy.require_dialect_match && step.dialect != plan.dialect {
            return Err(MigrationPlanError::DialectMismatch {
                step_id: step.id.clone(),
                plan_dialect: plan.dialect,
                step_dialect: step.dialect,
            });
        }
        if !plan.policy.allow_destructive_sql {
            validate_sql_non_destructive(&step.id, &step.sql)?;
        }
    }
    Ok(())
}

fn validate_sql_non_destructive(step_id: &str, sql: &str) -> MigrationPlanResult<()> {
    let normalized = normalize_sql_for_policy(sql);
    if normalized.contains(" drop table ") {
        return Err(MigrationPlanError::DestructiveSql {
            step_id: step_id.to_string(),
            reason: "DROP TABLE is not allowed".to_string(),
        });
    }
    if normalized.contains(" create or replace table ") || normalized.contains(" replace table ") {
        return Err(MigrationPlanError::DestructiveSql {
            step_id: step_id.to_string(),
            reason: "destructive table recreate is not allowed".to_string(),
        });
    }
    Ok(())
}

fn normalize_sql_for_policy(sql: &str) -> String {
    let mut normalized = String::with_capacity(sql.len() + 2);
    normalized.push(' ');
    let mut characters = sql.chars().peekable();

    while let Some(character) = characters.next() {
        match character {
            '-' if characters.peek() == Some(&'-') => {
                characters.next();
                for comment_character in characters.by_ref() {
                    if comment_character == '\n' {
                        break;
                    }
                }
                normalized.push(' ');
            }
            '/' if characters.peek() == Some(&'*') => {
                characters.next();
                let mut previous = '\0';
                for comment_character in characters.by_ref() {
                    if previous == '*' && comment_character == '/' {
                        break;
                    }
                    previous = comment_character;
                }
                normalized.push(' ');
            }
            '\'' => {
                consume_quoted(&mut characters, '\'');
                normalized.push(' ');
            }
            '"' => {
                consume_quoted(&mut characters, '"');
                normalized.push(' ');
            }
            other if other.is_ascii_alphanumeric() || other == '_' => {
                normalized.push(other.to_ascii_lowercase());
            }
            _ => normalized.push(' '),
        }
    }

    let compact = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(" {compact} ")
}

fn consume_quoted<I>(characters: &mut std::iter::Peekable<I>, quote: char)
where
    I: Iterator<Item = char>,
{
    while let Some(character) = characters.next() {
        if character == quote {
            if characters.peek() == Some(&quote) {
                characters.next();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migration_sql<'a>(
        migrations: &'a [EmbeddedMigration],
        version: &str,
    ) -> Result<&'a str, &'static str> {
        migrations
            .iter()
            .find(|migration| migration.version == version)
            .map(|migration| migration.sql)
            .ok_or("migration version is missing from the embedded catalog")
    }

    fn plan_with_sql(sql: &str) -> MigrationPlan {
        let mut plan = MigrationPlan::new(Dialect::Postgres);
        plan.push_step(MigrationStep::new("000001_test", Dialect::Postgres, sql));
        plan
    }

    #[test]
    fn postgres_is_the_only_parsed_dialect() -> MigrationPlanResult<()> {
        assert_eq!("postgres".parse::<Dialect>()?, Dialect::Postgres);
        assert!("sqlite".parse::<Dialect>().is_err());
        assert!("mysql".parse::<Dialect>().is_err());
        Ok(())
    }

    #[test]
    fn catalog_includes_latest_pricing_review_migrations() -> Result<(), &'static str> {
        let migrations = migrations_for_dialect(Dialect::Postgres);
        assert_eq!(
            migrations.last().map(|migration| migration.version),
            Some(LATEST_SCHEMA_VERSION)
        );
        assert_eq!(migrations.len(), 29);
        assert!(
            migrations
                .iter()
                .any(|migration| migration.sql.contains("api_key_quota_admissions"))
        );
        let accounting = migration_sql(&migrations, ACCOUNTING_CURRENCY_SCHEMA_VERSION)?;
        assert!(!accounting.contains("DEFAULT 'CNY'"));
        assert_eq!(
            accounting
                .matches("ADD COLUMN currency_code TEXT NOT NULL;")
                .count(),
            2
        );
        assert!(accounting.contains("project_wallets_station_credit_currency"));
        assert!(accounting.contains("credit_accounts_station_credit_currency"));
        assert!(accounting.contains("subscription_plans_station_credit_currency"));

        let audits = migration_sql(&migrations, PRICING_CHANGE_AUDITS_SCHEMA_VERSION)?;
        assert!(audits.contains("source_snapshot_id BIGINT"));
        assert!(audits.contains("source_change_set_id BIGINT"));

        let change_sets = migration_sql(&migrations, CHANGE_SETS_SCHEMA_VERSION)?;
        assert!(change_sets.contains("CREATE TABLE IF NOT EXISTS change_sets"));
        assert!(change_sets.contains("CREATE TABLE IF NOT EXISTS change_set_items"));
        assert!(change_sets.contains("CREATE TABLE IF NOT EXISTS change_set_events"));
        assert!(change_sets.contains("'provider_price', 'model_mapping', 'retail_price'"));
        assert!(change_sets.contains("'draft', 'pending_review', 'applied'"));
        assert!(change_sets.contains("change_set_events_append_only"));
        assert!(change_sets.contains("change_sets_activity"));
        assert!(change_sets.contains("status, updated_at DESC, id DESC"));
        assert!(accounting.contains("customer_charge_events_station_credit_currency"));
        assert!(accounting.contains("project_commercial_profiles_station_credit_currency"));
        assert!(accounting.contains("channel_model_prices_currency_code_iso"));
        assert!(accounting.contains("channel_model_price_versions_currency_code_iso"));
        assert_eq!(accounting.matches("^[A-Z]{3}$").count(), 2);

        let affinity = migration_sql(&migrations, ROUTE_AFFINITIES_SCHEMA_VERSION)?;
        assert!(affinity.contains("CREATE TABLE IF NOT EXISTS route_affinities"));
        assert!(affinity.contains("route_affinities_scope_unique"));
        assert!(affinity.contains("CHECK (key_hash ~ '^[0-9a-f]{64}$')"));
        Ok(())
    }

    #[test]
    fn fresh_catalog_uses_final_money_defaults_from_their_origin() -> Result<(), &'static str> {
        let migrations = migrations_for_dialect(Dialect::Postgres);

        let commercialization = migration_sql(&migrations, COMMERCIALIZATION_SCHEMA_VERSION)?;
        assert_eq!(commercialization.matches("DEFAULT 'CNY'").count(), 1);
        assert_eq!(
            commercialization
                .matches("DEFAULT 'STATION_CREDIT'")
                .count(),
            1
        );
        assert_eq!(
            migration_sql(&migrations, BALANCE_SUBSCRIPTION_SCHEMA_VERSION)?
                .matches("DEFAULT 'STATION_CREDIT'")
                .count(),
            2
        );
        assert!(
            migration_sql(&migrations, PROJECT_WALLET_SHADOW_SCHEMA_VERSION)?
                .contains("DEFAULT 'STATION_CREDIT'")
        );
        assert!(
            migration_sql(&migrations, PROJECT_COMMERCIALIZATION_SCHEMA_VERSION)?
                .contains("DEFAULT 'STATION_CREDIT'")
        );
        Ok(())
    }

    #[test]
    fn fresh_catalog_declares_subscription_relational_integrity() -> Result<(), &'static str> {
        let migrations = migrations_for_dialect(Dialect::Postgres);
        let balance = migration_sql(&migrations, BALANCE_SUBSCRIPTION_SCHEMA_VERSION)?;
        let grants = migration_sql(&migrations, SUBSCRIPTION_PROJECT_GRANTS_SCHEMA_VERSION)?;

        assert!(balance.contains("user_subscriptions_user_fkey"));
        assert!(balance.contains("FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE RESTRICT"));
        assert!(balance.contains("user_subscriptions_plan_fkey"));
        assert!(
            balance.contains(
                "FOREIGN KEY(plan_id) REFERENCES subscription_plans(id) ON DELETE RESTRICT"
            )
        );
        assert!(balance.contains("assignment_key TEXT NOT NULL"));
        assert!(balance.contains("assignment_request_snapshot JSONB NOT NULL"));
        assert!(balance.contains("user_subscriptions_assignment_key_key"));
        assert!(balance.contains("UNIQUE(assignment_key)"));
        assert!(balance.contains("user_subscriptions_assignment_key_normalized"));
        assert!(
            balance.contains(
                "CHECK (assignment_key <> '' AND assignment_key = BTRIM(assignment_key))"
            )
        );
        assert!(balance.contains("user_subscriptions_assignment_request_object"));
        assert!(balance.contains("CHECK (jsonb_typeof(assignment_request_snapshot) = 'object')"));
        assert!(balance.contains("CHECK (current_period_end > current_period_start)"));
        assert!(balance.contains("CHECK (assigned_interval_count > 0)"));
        assert!(grants.contains("user_subscription_quota_snapshot_owner_class_key"));
        assert!(grants.contains("UNIQUE(subscription_id, quota_class, id)"));
        assert!(grants.contains("subscription_entitlement_snapshot_owner_id_key"));
        assert!(grants.contains("UNIQUE(subscription_id, id)"));
        assert!(grants.contains("subscription_allowance_bucket_rule_owner_fkey"));
        assert!(
            grants.contains("FOREIGN KEY(subscription_id, quota_class, quota_rule_snapshot_id)")
        );
        assert!(grants.contains("subscription_allowance_bucket_entitlement_owner_fkey"));
        assert!(grants.contains("FOREIGN KEY(subscription_id, entitlement_snapshot_id)"));
        assert_eq!(
            grants
                .matches("ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED")
                .count(),
            3
        );
        assert!(!grants.contains(
            "CREATE INDEX IF NOT EXISTS user_subscription_quota_rule_snapshots_subscription"
        ));
        Ok(())
    }

    #[test]
    fn embedded_catalog_is_non_destructive() -> MigrationPlanResult<()> {
        let mut plan = MigrationPlan::new(Dialect::Postgres);
        for migration in EMBEDDED_MIGRATIONS {
            plan.push_step(MigrationStep::new(
                migration.version,
                Dialect::Postgres,
                migration.sql,
            ));
        }
        validate_plan_non_destructive(&plan)
    }

    #[test]
    fn rejects_drop_table_and_destructive_recreate() {
        assert!(matches!(
            validate_plan_non_destructive(&plan_with_sql("DROP TABLE users;")),
            Err(MigrationPlanError::DestructiveSql { .. })
        ));
        assert!(matches!(
            validate_plan_non_destructive(&plan_with_sql(
                "CREATE OR REPLACE TABLE users (id bigint primary key);"
            )),
            Err(MigrationPlanError::DestructiveSql { .. })
        ));
    }

    #[test]
    fn ignores_destructive_keywords_in_comments_and_strings() -> MigrationPlanResult<()> {
        validate_plan_non_destructive(&plan_with_sql(
            "-- DROP TABLE users;\nCREATE TABLE audit (message text default 'drop table users');",
        ))
    }

    #[test]
    fn ddl_splitter_keeps_dollar_quoted_bodies_together() {
        let statements = split_ddl_statements(
            "CREATE FUNCTION f() RETURNS trigger AS $$\nBEGIN\n  RETURN NEW;\nEND;\n$$ LANGUAGE plpgsql;\nCREATE INDEX i ON t(id);",
        );
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("RETURN NEW;"));
        assert!(statements[1].starts_with("CREATE INDEX"));
    }
}

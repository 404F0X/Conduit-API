use std::{
    env, fmt,
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use clap::{CommandFactory, Parser, Subcommand};
use conduit_auth::password::{DEFAULT_BCRYPT_COST, encode_password_bcrypt_hex};
use conduit_config::{AppConfig, ConfigError, load_default_search, load_from_path, validate};
use conduit_db::connection::connect_postgres;
use conduit_db::{DatabaseConfig, DbDialect};
use conduit_http::{AppState, serve_listener_with_graceful_timeout, shutdown_signal};
use serde_yaml::{Mapping, Value};
use tokio::net::TcpListener;
use tokio::runtime::Builder;

const CONFIG_LOAD_FAILED: u8 = 2;
const CONFIG_VALIDATE_FAILED: u8 = 3;
const UNKNOWN_CONFIG_KEY: u8 = 4;
const SERVER_START_FAILED: u8 = 10;

const SUPPORTED_CONFIG_KEYS: &[&str] = &[
    "server.port",
    "server.name",
    "server.base_path",
    "server.debug",
    "db.dialect",
    "db.dsn",
];

#[derive(Debug, Parser)]
#[command(
    name = "conduit",
    about = "Conduit API command line interface",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[arg(long, default_value = "config.yml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(subcommand)]
    Config(ConfigCommand),
    #[command(subcommand)]
    Admin(AdminCommand),
    Version,
    BuildInfo,
    Help,
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Reset an existing system owner's password. The new password is read
    /// from CONDUIT_ADMIN_RESET_PASSWORD and is never accepted on argv.
    ResetPassword { email: Option<String> },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Preview,
    Validate,
    Get { key: String },
}

#[derive(Debug)]
pub enum CliError {
    ConfigLoad(String),
    ConfigValidate(String),
    UnknownConfigKey(String),
    ServerStart(String),
}

impl CliError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ConfigLoad(_) => CONFIG_LOAD_FAILED,
            Self::ConfigValidate(_) => CONFIG_VALIDATE_FAILED,
            Self::UnknownConfigKey(_) => UNKNOWN_CONFIG_KEY,
            Self::ServerStart(_) => SERVER_START_FAILED,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConfigLoad(message) => write!(f, "config load failed: {message}"),
            Self::ConfigValidate(message) => write!(f, "config validate failed: {message}"),
            Self::UnknownConfigKey(key) => write!(
                f,
                "unknown config key: {key}\nsupported keys: {}",
                SUPPORTED_CONFIG_KEYS.join(", ")
            ),
            Self::ServerStart(message) => write!(f, "server start failed: {message}"),
        }
    }
}

// 显式声明 Error trait——本类型仅手动实现 Display，无内部 source 链。
impl std::error::Error for CliError {}

pub fn run() -> Result<(), CliError> {
    run_with_writer(Cli::parse(), &mut io::stdout())
}

fn run_with_writer<W>(cli: Cli, output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    run_with_writer_and_server(cli, output, start_http_server)
}

fn run_with_writer_and_server<W, F>(
    cli: Cli,
    output: &mut W,
    start_server: F,
) -> Result<(), CliError>
where
    W: Write,
    F: FnOnce(AppConfig) -> Result<(), String>,
{
    match cli.command {
        Some(Commands::Config(command)) => run_config(&cli.config, command, output),
        Some(Commands::Admin(command)) => run_admin(&cli.config, command, output),
        Some(Commands::Version) => {
            writeln!(output, "{}", build_version())
                .map_err(|err| CliError::ServerStart(err.to_string()))?;
            Ok(())
        }
        Some(Commands::BuildInfo) => {
            write_build_info(output)?;
            Ok(())
        }
        Some(Commands::Help) => print_help(output),
        None => start_server_from_config(&cli.config, start_server),
    }
}

fn run_admin<W>(config_path: &Path, command: AdminCommand, output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    let config = load_config(config_path)?;
    validate_config(&config)?;
    match command {
        AdminCommand::ResetPassword { email } => {
            let password = env::var("CONDUIT_ADMIN_RESET_PASSWORD").map_err(|_| {
                CliError::ServerStart(
                    "CONDUIT_ADMIN_RESET_PASSWORD must be set for admin reset-password".into(),
                )
            })?;
            if password.len() < 8 {
                return Err(CliError::ServerStart(
                    "admin reset password must contain at least 8 characters".into(),
                ));
            }
            let normalized_email = email
                .map(|value| value.trim().to_lowercase())
                .filter(|value| !value.is_empty());
            let cost = if config.api_auth.bcrypt_cost == 0 {
                DEFAULT_BCRYPT_COST
            } else {
                config.api_auth.bcrypt_cost
            };
            let hash = encode_password_bcrypt_hex(&password, cost)
                .map_err(|error| CliError::ServerStart(error.to_string()))?;
            let reset_email = Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| CliError::ServerStart(error.to_string()))?
                .block_on(reset_owner_password(
                    &config,
                    normalized_email.as_deref(),
                    &hash,
                ))?;
            writeln!(output, "owner password reset for {reset_email}")
                .map_err(|error| CliError::ServerStart(error.to_string()))?;
            Ok(())
        }
    }
}

async fn reset_owner_password(
    config: &AppConfig,
    email: Option<&str>,
    password_hash: &str,
) -> Result<String, CliError> {
    let dialect = DbDialect::from_str(&config.db.dialect)
        .map_err(|error| CliError::ServerStart(error.to_string()))?;
    let mut db_config = DatabaseConfig::new(dialect, &config.db.dsn);
    db_config.max_connections = config.db.max_open_conns;
    db_config.min_connections = config.db.max_idle_conns.min(config.db.max_open_conns);
    db_config.connect_timeout = config.db.connect_timeout;
    db_config.conn_max_lifetime = config.db.conn_max_lifetime;
    db_config.conn_max_idle_time = config.db.conn_max_idle_time;
    if dialect != DbDialect::Postgres {
        return Err(CliError::ServerStart(format!(
            "database dialect {dialect} is no longer supported; configure db.dialect=postgres"
        )));
    }
    reset_owner_password_postgres(&db_config, email, password_hash).await
}

async fn reset_owner_password_postgres(
    db_config: &DatabaseConfig,
    email: Option<&str>,
    password_hash: &str,
) -> Result<String, CliError> {
    let pool = connect_postgres(db_config)
        .await
        .map_err(|error| CliError::ServerStart(error.to_string()))?;
    let email = if let Some(email) = email {
        email.to_string()
    } else {
        let owners = sqlx::query_scalar::<_, String>(
            "SELECT email FROM users WHERE is_owner = TRUE AND deleted_at = 0 ORDER BY id LIMIT 2",
        )
        .fetch_all(&pool)
        .await
        .map_err(|error| CliError::ServerStart(error.to_string()))?;
        match owners.as_slice() {
            [email] => email.clone(),
            [] => {
                pool.close().await;
                return Err(CliError::ServerStart(
                    "no active owner account exists".into(),
                ));
            }
            _ => {
                pool.close().await;
                return Err(CliError::ServerStart(
                    "multiple active owners exist; provide the owner email".into(),
                ));
            }
        }
    };
    let result = sqlx::query(
        "UPDATE users SET password = $1, updated_at = $2 \
         WHERE lower(email) = lower($3) AND is_owner = TRUE AND deleted_at = 0",
    )
    .bind(password_hash)
    .bind(chrono::Utc::now())
    .bind(&email)
    .execute(&pool)
    .await
    .map_err(|error| CliError::ServerStart(error.to_string()))?;
    pool.close().await;
    if result.rows_affected() == 1 {
        Ok(email)
    } else {
        Err(CliError::ServerStart(format!(
            "active owner account not found for {email}"
        )))
    }
}

fn run_config<W>(config_path: &Path, command: ConfigCommand, output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    let config = load_config(config_path)?;
    run_config_with_loaded(config, command, output)
}

fn run_config_with_loaded<W>(
    config: AppConfig,
    command: ConfigCommand,
    output: &mut W,
) -> Result<(), CliError>
where
    W: Write,
{
    match command {
        ConfigCommand::Preview => {
            validate_config(&config)?;
            print_config_preview(&config, output)
        }
        ConfigCommand::Validate => {
            validate_config(&config)?;
            writeln!(output, "config valid")
                .map_err(|err| CliError::ServerStart(err.to_string()))?;
            Ok(())
        }
        ConfigCommand::Get { key } => {
            validate_config(&config)?;
            match config_value(&config, &key) {
                Some(value) => {
                    writeln!(output, "{value}")
                        .map_err(|err| CliError::ServerStart(err.to_string()))?;
                    Ok(())
                }
                None => Err(CliError::UnknownConfigKey(key)),
            }
        }
    }
}

fn print_help<W>(output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    Cli::command()
        .write_long_help(output)
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(output).map_err(|err| CliError::ServerStart(err.to_string()))?;
    Ok(())
}

fn start_server_from_config<F>(config_path: &Path, start_server: F) -> Result<(), CliError>
where
    F: FnOnce(AppConfig) -> Result<(), String>,
{
    let config = load_config(config_path)?;
    validate_config(&config)?;
    // P-50: the previous `StartupPlan::validate_order()` here was pure
    // decoration — it validated a hard-coded stage list whose "passing" implied
    // subsystems started in that order, but the real startup is the linear code
    // in `start_http_server_async` and was never driven by the plan (e.g. the
    // "Scheduler" stage passed while only GC jobs — not the business workers,
    // see P-02 — actually started). A validation that lies is worse than none,
    // so the decorative plan was removed; startup order now has one source of
    // truth. If the scheduler workers land (P-02), reintroduce a plan that
    // *drives* each stage rather than merely asserting an order.
    start_server(config).map_err(CliError::ServerStart)
}

fn start_http_server(config: AppConfig) -> Result<(), String> {
    let _logging_guard = crate::runtime_logging::init(&config.log)?;
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?
        .block_on(start_http_server_async(config))
}

async fn start_http_server_async(config: AppConfig) -> Result<(), String> {
    let addr = format!("{}:{}", config.server.host, config.server.port);
    let graceful_shutdown_timeout = config.server.graceful_shutdown_timeout;
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|err| format!("failed to bind {addr}: {err}"))?;

    // Build the wired services (DB connect + migrate + orchestrator/admin-schema
    // wiring) over the configured database, then hand them to AppState so the
    // router serves real handlers instead of the bare 5xx fallbacks. Without
    // this, AppServices::default() leaves every service slot None.
    let (services, pools, live_registry) = crate::wiring::build_runtime_services(&config).await?;
    tracing::info!("PostgreSQL runtime and maintenance workers are active");
    let maintenance = crate::maintenance::start_postgres(
        pools.master_clone(),
        &config.gc,
        &config.provider_quota,
        live_registry,
    )
    .await?;
    let metrics_config = config.metrics.clone();
    let state = AppState::new(Arc::new(config), Arc::new(services));
    let metrics_state = state.metrics().clone();

    let metrics_task = if metrics_config.enabled {
        let metrics_addr = format!("{}:{}", metrics_config.host, metrics_config.port);
        let metrics_listener = TcpListener::bind(&metrics_addr)
            .await
            .map_err(|err| format!("failed to bind metrics listener {metrics_addr}: {err}"))?;
        let metrics_app = conduit_http::metrics_router(metrics_state, &metrics_config.path);
        Some(tokio::spawn(async move {
            axum::serve(metrics_listener, metrics_app).await
        }))
    } else {
        None
    };

    let result = serve_listener_with_graceful_timeout(
        listener,
        state,
        async {
            let _ = shutdown_signal().await;
        },
        graceful_shutdown_timeout,
    )
    .await;
    if let Some(task) = metrics_task {
        task.abort();
        let _ = task.await;
    }
    maintenance.shutdown().await;
    result.map_err(|err| format!("http server failed: {err}"))
}

fn build_version() -> &'static str {
    option_env!("CONDUIT_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn write_build_info<W>(output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "version: {}", build_version())
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(output, "commit: {}", build_value("CONDUIT_BUILD_COMMIT"))
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(output, "branch: {}", build_value("CONDUIT_BUILD_BRANCH"))
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(output, "build_time: {}", build_value("CONDUIT_BUILD_TIME"))
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(
        output,
        "rustc_version: {}",
        build_value("CONDUIT_BUILD_RUSTC_VERSION")
    )
    .map_err(|err| CliError::ServerStart(err.to_string()))?;
    writeln!(output, "target: {}", build_value("CONDUIT_BUILD_TARGET"))
        .map_err(|err| CliError::ServerStart(err.to_string()))?;
    Ok(())
}

fn build_value(key: &str) -> &'static str {
    match key {
        "CONDUIT_BUILD_COMMIT" => option_env!("CONDUIT_BUILD_COMMIT").unwrap_or("unknown"),
        "CONDUIT_BUILD_BRANCH" => option_env!("CONDUIT_BUILD_BRANCH").unwrap_or("unknown"),
        "CONDUIT_BUILD_TIME" => option_env!("CONDUIT_BUILD_TIME").unwrap_or("unknown"),
        "CONDUIT_BUILD_RUSTC_VERSION" => {
            option_env!("CONDUIT_BUILD_RUSTC_VERSION").unwrap_or("unknown")
        }
        "CONDUIT_BUILD_TARGET" => option_env!("CONDUIT_BUILD_TARGET").unwrap_or("unknown"),
        _ => "unknown",
    }
}

fn load_config(path: &Path) -> Result<AppConfig, CliError> {
    let result = if path == Path::new("config.yml") && !path.exists() {
        load_default_search()
    } else {
        load_from_path(path)
    };
    result.map_err(map_config_error)
}

fn validate_config(config: &AppConfig) -> Result<(), CliError> {
    validate(config).map_err(|err| CliError::ConfigValidate(err.to_string()))
}

fn map_config_error(err: ConfigError) -> CliError {
    match err {
        ConfigError::Validation(err) => CliError::ConfigValidate(err.to_string()),
        other => CliError::ConfigLoad(other.to_string()),
    }
}

fn print_config_preview<W>(config: &AppConfig, output: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    let mut value =
        serde_yaml::to_value(config).map_err(|err| CliError::ConfigLoad(err.to_string()))?;
    mask_secrets(&mut value);
    let preview =
        serde_yaml::to_string(&value).map_err(|err| CliError::ConfigLoad(err.to_string()))?;
    write!(output, "{preview}").map_err(|err| CliError::ServerStart(err.to_string()))?;
    Ok(())
}

fn config_value(config: &AppConfig, key: &str) -> Option<String> {
    match key {
        "server.port" => Some(config.server.port.to_string()),
        "server.name" => Some(config.server.name.clone()),
        "server.base_path" => Some(config.server.base_path.clone()),
        "server.debug" => Some(config.server.debug.to_string()),
        "db.dialect" => Some(config.db.dialect.clone()),
        "db.dsn" => Some(config.db.dsn.clone()),
        _ => None,
    }
}

fn mask_secrets(value: &mut Value) {
    match value {
        Value::Mapping(mapping) => mask_mapping(mapping),
        Value::Sequence(values) => {
            for value in values {
                mask_secrets(value);
            }
        }
        _ => {}
    }
}

fn mask_mapping(mapping: &mut Mapping) {
    for (key, value) in mapping {
        let key = key.as_str().unwrap_or_default().to_ascii_lowercase();
        if is_secret_key(&key) {
            *value = Value::String("***".to_string());
        } else {
            mask_secrets(value);
        }
    }
}

fn is_secret_key(key: &str) -> bool {
    ["secret", "key", "password", "token"]
        .iter()
        .any(|needle| key.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_output(cli: Cli) -> Result<String, CliError> {
        snapshot_output_with_server(cli, |_| Ok(()))
    }

    fn snapshot_output_with_server<F>(cli: Cli, start_server: F) -> Result<String, CliError>
    where
        F: FnOnce(AppConfig) -> Result<(), String>,
    {
        let mut output = Vec::new();
        run_with_writer_and_server(cli, &mut output, start_server)?;
        String::from_utf8(output).map_err(|err| CliError::ServerStart(err.to_string()))
    }

    fn test_cli(command: Option<Commands>) -> Cli {
        Cli {
            config: PathBuf::from("config.yml"),
            command,
        }
    }

    #[test]
    fn config_get_supports_go_compatible_keys() {
        let config = AppConfig::default();

        for key in SUPPORTED_CONFIG_KEYS {
            assert!(config_value(&config, key).is_some(), "{key}");
        }
    }

    #[test]
    fn unknown_key_uses_fixed_exit_code() {
        let err = CliError::UnknownConfigKey("server.missing".to_string());

        assert_eq!(err.exit_code(), UNKNOWN_CONFIG_KEY);
        assert!(err.to_string().contains("supported keys:"));
    }

    #[test]
    fn version_output_is_only_package_version() {
        assert_eq!(build_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn snapshot_version_output() -> Result<(), Box<dyn std::error::Error>> {
        let output = snapshot_output(test_cli(Some(Commands::Version)))?;

        assert_eq!(output, format!("{}\n", env!("CARGO_PKG_VERSION")));
        Ok(())
    }

    #[test]
    fn snapshot_build_info_fields() -> Result<(), Box<dyn std::error::Error>> {
        let output = snapshot_output(test_cli(Some(Commands::BuildInfo)))?;
        let lines = output.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], format!("version: {}", build_version()));
        assert_build_info_field(lines[1], "commit");
        assert_build_info_field(lines[2], "branch");
        assert_build_info_field(lines[3], "build_time");
        assert_build_info_field(lines[4], "rustc_version");
        assert_build_info_field(lines[5], "target");
        Ok(())
    }

    #[test]
    fn snapshot_unknown_key_error() {
        let err = CliError::UnknownConfigKey("server.missing".to_string());

        assert_eq!(
            err.to_string(),
            "unknown config key: server.missing\nsupported keys: server.port, server.name, server.base_path, server.debug, db.dialect, db.dsn"
        );
    }

    #[test]
    fn snapshot_config_get_server_port_output() -> Result<(), Box<dyn std::error::Error>> {
        let mut output = Vec::new();
        run_config_with_loaded(
            AppConfig::default(),
            ConfigCommand::Get {
                key: "server.port".to_string(),
            },
            &mut output,
        )?;

        assert_eq!(String::from_utf8(output)?, "8090\n");
        Ok(())
    }

    #[test]
    fn default_command_validates_then_starts_http_server() -> Result<(), Box<dyn std::error::Error>>
    {
        let output = snapshot_output_with_server(test_cli(None), |config| {
            assert_eq!(config.server.port, 8090);
            Ok(())
        })?;

        assert_eq!(output, "");
        Ok(())
    }

    #[test]
    fn server_start_failure_uses_fixed_exit_code() {
        // 用 match 显式断言失败分支，避免使用被 workspace 禁用的 unwrap_err。
        let err =
            match snapshot_output_with_server(test_cli(None), |_| Err("bind failed".to_string())) {
                Ok(_) => panic!("expected server start failure, got success"),
                Err(err) => err,
            };

        assert_eq!(err.exit_code(), SERVER_START_FAILED);
        assert_eq!(err.to_string(), "server start failed: bind failed");
    }

    fn assert_build_info_field(line: &str, field: &str) {
        let prefix = format!("{field}: ");
        assert!(line.starts_with(&prefix), "{line}");
        assert!(line.len() > prefix.len(), "{line}");
    }

    #[tokio::test]
    async fn postgres_owner_password_reset_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let mut db_config = DatabaseConfig::new(DbDialect::Postgres, dsn);
        db_config.max_connections = 2;
        db_config.min_connections = 0;
        let pool = connect_postgres(&db_config).await?;
        conduit_db::connection::migrate_postgres(&pool).await?;
        let email = format!("pg-reset-{}@example.test", std::process::id());
        sqlx::query(
            "INSERT INTO users (email, password, status, is_owner, scopes) \
             VALUES ($1, 'old', 'activated', TRUE, '[]'::jsonb)",
        )
        .bind(&email)
        .execute(&pool)
        .await?;
        drop(pool);

        let reset = reset_owner_password_postgres(&db_config, Some(&email), "new-hash").await?;
        assert_eq!(reset, email);
        let pool = connect_postgres(&db_config).await?;
        let saved: String = sqlx::query_scalar("SELECT password FROM users WHERE email = $1")
            .bind(&email)
            .fetch_one(&pool)
            .await?;
        assert_eq!(saved, "new-hash");
        sqlx::query("DELETE FROM users WHERE email = $1")
            .bind(&email)
            .execute(&pool)
            .await?;
        pool.close().await;
        Ok(())
    }
}

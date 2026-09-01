use crate::model::{AppConfig, OidcProviderConfig, duration_format};
use crate::validate::{ValidationError, validate};
use serde::de::DeserializeOwned;
use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_CONFIG_FILE: &str = "config.yml";

type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub server_port: Option<u16>,
}

impl CliOverrides {
    pub fn apply(&self, config: &mut AppConfig) {
        if let Some(port) = self.server_port {
            config.server.port = port;
        }
    }
}

pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    Env {
        key: &'static str,
        message: String,
    },
    Validation(ValidationError),
}

impl Display for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "failed to read {}: {source}", path.display()),
            Self::Yaml { path, source } => {
                write!(
                    f,
                    "failed to parse YAML config {}: {source}",
                    path.display()
                )
            }
            Self::Env { key, message } => write!(f, "invalid env {key}: {message}"),
            Self::Validation(err) => write!(f, "{err}"),
        }
    }
}

// Configuration errors are routinely logged with both Display and Debug.
// Environment values can contain database passwords, JWT keys, or OIDC client
// secrets, so ConfigError deliberately never retains or renders them.
impl std::fmt::Debug for ConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Yaml { source, .. } => Some(source),
            Self::Validation(err) => Some(err),
            Self::Env { .. } => None,
        }
    }
}

impl From<ValidationError> for ConfigError {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}

pub fn load_default_search() -> Result<AppConfig, ConfigError> {
    load_default_search_with_cli(&CliOverrides::default())
}

pub fn load_default_search_with_cli(cli: &CliOverrides) -> Result<AppConfig, ConfigError> {
    let path = discover_config_file();
    load_from_optional_path_with_cli(path.as_deref(), cli)
}

pub fn load_from_path(path: impl AsRef<Path>) -> Result<AppConfig, ConfigError> {
    load_from_path_with_cli(path, &CliOverrides::default())
}

pub fn load_from_path_with_cli(
    path: impl AsRef<Path>,
    cli: &CliOverrides,
) -> Result<AppConfig, ConfigError> {
    load_from_optional_path_with_cli(Some(path.as_ref()), cli)
}

pub fn load_from_optional_path(path: Option<&Path>) -> Result<AppConfig, ConfigError> {
    load_from_optional_path_with_cli(path, &CliOverrides::default())
}

pub fn load_from_optional_path_with_cli(
    path: Option<&Path>,
    cli: &CliOverrides,
) -> Result<AppConfig, ConfigError> {
    let env_lookup = |key: &str| env::var(key).ok();
    load_from_optional_path_with_env(path, cli, &env_lookup)
}

fn load_from_optional_path_with_env(
    path: Option<&Path>,
    cli: &CliOverrides,
    env_lookup: EnvLookup<'_>,
) -> Result<AppConfig, ConfigError> {
    // Merge order is intentionally explicit for the CLI crate:
    // defaults < optional config.yml < CONDUIT_* env < CLI flags.
    let mut config = if let Some(path) = path {
        read_yaml_config(path)?
    } else {
        AppConfig::default()
    };
    apply_env_overrides(&mut config, env_lookup)?;
    cli.apply(&mut config);
    validate(&config)?;
    Ok(config)
}

pub fn discover_config_file() -> Option<PathBuf> {
    search_paths()
        .into_iter()
        .map(|dir| dir.join(DEFAULT_CONFIG_FILE))
        .find(|path| path.is_file())
}

pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("."), PathBuf::from("/etc/conduit")];
    if let Some(home) = home_dir() {
        paths.push(home.join(".config").join("conduit"));
    }
    paths.push(PathBuf::from("./conf"));
    paths
}

fn read_yaml_config(path: &Path) -> Result<AppConfig, ConfigError> {
    let content = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_yaml::from_str::<AppConfig>(&content).map_err(|source| ConfigError::Yaml {
        path: path.to_path_buf(),
        source,
    })
}

fn apply_env_overrides(
    config: &mut AppConfig,
    env_lookup: EnvLookup<'_>,
) -> Result<(), ConfigError> {
    set_string(env_lookup, "CONDUIT_SERVER_NAME", &mut config.server.name)?;
    set_string(env_lookup, "CONDUIT_SERVER_HOST", &mut config.server.host)?;
    set_string(
        env_lookup,
        "CONDUIT_SERVER_PUBLIC_URL",
        &mut config.server.public_url,
    )?;
    set_u16(env_lookup, "CONDUIT_SERVER_PORT", &mut config.server.port)?;
    set_string(
        env_lookup,
        "CONDUIT_SERVER_BASE_PATH",
        &mut config.server.base_path,
    )?;
    set_string_vec(
        env_lookup,
        "CONDUIT_SERVER_TRUSTED_PROXIES",
        &mut config.server.trusted_proxies,
    )?;
    set_bool(env_lookup, "CONDUIT_SERVER_DEBUG", &mut config.server.debug)?;
    set_bool(
        env_lookup,
        "CONDUIT_SERVER_DISABLE_SSL_VERIFY",
        &mut config.server.disable_ssl_verify,
    )?;
    set_string_vec(
        env_lookup,
        "CONDUIT_SERVER_CORS_ALLOWED_ORIGINS",
        &mut config.server.cors.allowed_origins,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_SERVER_READ_TIMEOUT",
        &mut config.server.read_timeout,
    )?;

    set_string(env_lookup, "CONDUIT_DB_DIALECT", &mut config.db.dialect)?;
    set_string(env_lookup, "CONDUIT_DB_DSN", &mut config.db.dsn)?;
    set_bool(env_lookup, "CONDUIT_DB_DEBUG", &mut config.db.debug)?;
    set_u32(
        env_lookup,
        "CONDUIT_DB_MAX_OPEN_CONNS",
        &mut config.db.max_open_conns,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_DB_MAX_IDLE_CONNS",
        &mut config.db.max_idle_conns,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_DB_CONN_MAX_LIFETIME",
        &mut config.db.conn_max_lifetime,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_DB_CONN_MAX_IDLE_TIME",
        &mut config.db.conn_max_idle_time,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_DB_DISABLE_AUTO_MIGRATION",
        &mut config.db.disable_auto_migration,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_DB_CONNECT_TIMEOUT",
        &mut config.db.connect_timeout,
    )?;
    set_string(
        env_lookup,
        "CONDUIT_DB_READ_REPLICA_READ_DSN",
        &mut config.db.read_replica.read_dsn,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_DB_READ_REPLICA_READ_MAX_OPEN_CONNS",
        &mut config.db.read_replica.read_max_open_conns,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_DB_READ_REPLICA_READ_MAX_IDLE_CONNS",
        &mut config.db.read_replica.read_max_idle_conns,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_DB_READ_REPLICA_FALLBACK_ON_REPLICA_FAILURE",
        &mut config.db.read_replica.fallback_on_replica_failure,
    )?;

    set_string(env_lookup, "CONDUIT_LOG_NAME", &mut config.log.name)?;
    set_string(env_lookup, "CONDUIT_LOG_LEVEL", &mut config.log.level)?;
    set_string(env_lookup, "CONDUIT_LOG_FORMAT", &mut config.log.format)?;
    set_string(
        env_lookup,
        "CONDUIT_LOG_DIRECTORY",
        &mut config.log.directory,
    )?;
    set_bool(env_lookup, "CONDUIT_LOG_STDOUT", &mut config.log.stdout)?;

    set_bool(
        env_lookup,
        "CONDUIT_METRICS_ENABLED",
        &mut config.metrics.enabled,
    )?;
    set_string(env_lookup, "CONDUIT_METRICS_HOST", &mut config.metrics.host)?;
    set_u16(env_lookup, "CONDUIT_METRICS_PORT", &mut config.metrics.port)?;
    set_string(env_lookup, "CONDUIT_METRICS_PATH", &mut config.metrics.path)?;

    set_string(env_lookup, "CONDUIT_CACHE_MODE", &mut config.cache.mode)?;
    set_u64(
        env_lookup,
        "CONDUIT_CACHE_MEMORY_MAX_CAPACITY",
        &mut config.cache.memory.max_capacity,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_CACHE_MEMORY_TTL",
        &mut config.cache.memory.ttl,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_CACHE_ROUTE_AFFINITY_ENABLED",
        &mut config.cache.route_affinity.enabled,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_CACHE_ROUTE_AFFINITY_PROMPT_CACHE_TTL",
        &mut config.cache.route_affinity.prompt_cache_ttl,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_CACHE_ROUTE_AFFINITY_RESPONSE_CONTINUITY_TTL",
        &mut config.cache.route_affinity.response_continuity_ttl,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_CACHE_ROUTE_AFFINITY_LOOKUP_CACHE_TTL",
        &mut config.cache.route_affinity.lookup_cache_ttl,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_CACHE_ROUTE_AFFINITY_NEGATIVE_CACHE_TTL",
        &mut config.cache.route_affinity.negative_cache_ttl,
    )?;
    set_optional_string(
        env_lookup,
        "CONDUIT_CACHE_REDIS_URL",
        &mut config.cache.redis.url,
    )?;
    set_string(
        env_lookup,
        "CONDUIT_CACHE_REDIS_ADDR",
        &mut config.cache.redis.addr,
    )?;
    set_string_vec(
        env_lookup,
        "CONDUIT_CACHE_REDIS_ADDRS",
        &mut config.cache.redis.addrs,
    )?;
    set_optional_string(
        env_lookup,
        "CONDUIT_CACHE_REDIS_USERNAME",
        &mut config.cache.redis.username,
    )?;
    set_optional_string(
        env_lookup,
        "CONDUIT_CACHE_REDIS_PASSWORD",
        &mut config.cache.redis.password,
    )?;
    set_u8(
        env_lookup,
        "CONDUIT_CACHE_REDIS_DB",
        &mut config.cache.redis.db,
    )?;
    set_optional_string(
        env_lookup,
        "CONDUIT_CACHE_REDIS_MASTER_NAME",
        &mut config.cache.redis.master_name,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_CACHE_REDIS_SENTINEL",
        &mut config.cache.redis.sentinel,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_CACHE_REDIS_TLS",
        &mut config.cache.redis.tls,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_CACHE_REDIS_CLUSTER",
        &mut config.cache.redis.cluster,
    )?;

    set_bool(env_lookup, "CONDUIT_GC_ENABLED", &mut config.gc.enabled)?;
    set_bool(
        env_lookup,
        "CONDUIT_GC_STALE_PROCESSING_ENABLED",
        &mut config.gc.stale_processing_enabled,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_GC_STALE_PROCESSING_INTERVAL",
        &mut config.gc.stale_processing_interval,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_GC_REQUESTS_CLEANUP_ENABLED",
        &mut config.gc.requests_cleanup_enabled,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_GC_REQUESTS_RETENTION",
        &mut config.gc.requests_retention,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_GC_USAGE_LOGS_CLEANUP_ENABLED",
        &mut config.gc.usage_logs_cleanup_enabled,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_GC_USAGE_LOGS_RETENTION",
        &mut config.gc.usage_logs_retention,
    )?;

    set_bool(
        env_lookup,
        "CONDUIT_PROVIDER_QUOTA_ENABLED",
        &mut config.provider_quota.enabled,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_PROVIDER_QUOTA_CHECK_INTERVAL",
        &mut config.provider_quota.check_interval,
    )?;
    set_f64(
        env_lookup,
        "CONDUIT_PROVIDER_QUOTA_WARNING_RATIO",
        &mut config.provider_quota.warning_ratio,
    )?;
    set_string_vec(
        env_lookup,
        "CONDUIT_PROVIDER_QUOTA_PROVIDERS",
        &mut config.provider_quota.providers,
    )?;

    set_bool(env_lookup, "CONDUIT_OIDC_ENABLED", &mut config.oidc.enabled)?;
    set_optional_string(
        env_lookup,
        "CONDUIT_OIDC_REDIRECT_BASE_URL",
        &mut config.oidc.redirect_base_url,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_OIDC_STATE_TTL",
        &mut config.oidc.state_ttl,
    )?;
    set_json_or_csv(
        env_lookup,
        "CONDUIT_OIDC_PROVIDERS",
        &mut config.oidc.providers,
    )?;

    set_optional_string(
        env_lookup,
        "CONDUIT_API_AUTH_JWT_SECRET",
        &mut config.api_auth.jwt_secret,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_API_AUTH_SESSION_TTL",
        &mut config.api_auth.session_ttl,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_API_AUTH_BCRYPT_COST",
        &mut config.api_auth.bcrypt_cost,
    )?;

    set_bool(
        env_lookup,
        "CONDUIT_RETRY_ENABLED",
        &mut config.retry.enabled,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_RETRY_MAX_CHANNEL_RETRIES",
        &mut config.retry.max_channel_retries,
    )?;
    set_u32(
        env_lookup,
        "CONDUIT_RETRY_MAX_SINGLE_CHANNEL_RETRIES",
        &mut config.retry.max_single_channel_retries,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_RETRY_RETRY_DELAY",
        &mut config.retry.retry_delay,
    )?;
    set_string(
        env_lookup,
        "CONDUIT_RETRY_STRATEGY",
        &mut config.retry.strategy,
    )?;
    set_bool(
        env_lookup,
        "CONDUIT_RETRY_UPSTREAM_ERROR_PASSTHROUGH",
        &mut config.retry.upstream_error_passthrough,
    )?;
    set_duration(
        env_lookup,
        "CONDUIT_RETRY_TIMEOUT",
        &mut config.retry.timeout,
    )?;

    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn set_string(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut String,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = value;
    }
    Ok(())
}

fn set_optional_string(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut Option<String>,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = if value.trim().is_empty() {
            None
        } else {
            Some(value)
        };
    }
    Ok(())
}

fn set_bool(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut bool,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_bool(key, &value)?;
    }
    Ok(())
}

fn set_u8(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut u8,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_number(key, &value)?;
    }
    Ok(())
}

fn set_u16(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut u16,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_number(key, &value)?;
    }
    Ok(())
}

fn set_u32(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut u32,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_number(key, &value)?;
    }
    Ok(())
}

fn set_u64(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut u64,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_number(key, &value)?;
    }
    Ok(())
}

fn set_f64(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut f64,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_number(key, &value)?;
    }
    Ok(())
}

fn set_duration(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut Duration,
) -> Result<(), ConfigError> {
    if let Some(value) = env_value(env_lookup, key) {
        *target = duration_format::parse_duration(&value).map_err(|_| ConfigError::Env {
            key,
            message: "expected a duration such as 500ms, 30s, or 5m".to_string(),
        })?;
    }
    Ok(())
}

fn set_string_vec(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut Vec<String>,
) -> Result<(), ConfigError> {
    set_json_or_csv(env_lookup, key, target)
}

fn set_json_or_csv<T>(
    env_lookup: EnvLookup<'_>,
    key: &'static str,
    target: &mut Vec<T>,
) -> Result<(), ConfigError>
where
    T: DeserializeOwned,
{
    if let Some(value) = env_value(env_lookup, key) {
        *target = parse_json_or_csv(key, &value)?;
    }
    Ok(())
}

fn parse_json_or_csv<T>(key: &'static str, value: &str) -> Result<Vec<T>, ConfigError>
where
    T: DeserializeOwned,
{
    match serde_json::from_str::<Vec<T>>(value) {
        Ok(parsed) => Ok(parsed),
        Err(_) => {
            let csv_value = value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| serde_json::Value::String(part.to_string()))
                .collect::<Vec<_>>();
            serde_json::from_value(serde_json::Value::Array(csv_value)).map_err(|_| {
                ConfigError::Env {
                    key,
                    message: "expected a JSON array or compatible comma-separated list".to_string(),
                }
            })
        }
    }
}

fn parse_bool(key: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "no" | "n" | "off" => Ok(false),
        _ => Err(ConfigError::Env {
            key,
            message: "expected boolean value".to_string(),
        }),
    }
}

fn parse_number<T>(key: &'static str, value: &str) -> Result<T, ConfigError>
where
    T: std::str::FromStr,
    T::Err: Display,
{
    value.trim().parse::<T>().map_err(|err| ConfigError::Env {
        key,
        message: err.to_string(),
    })
}

fn env_value(env_lookup: EnvLookup<'_>, key: &str) -> Option<String> {
    env_lookup(key)
}

#[allow(dead_code)]
fn _assert_oidc_provider_env_supported(_: Vec<OidcProviderConfig>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_overrides_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.yml");
        fs::write(
            &path,
            r#"
server:
  port: 9001
db:
  dsn: postgresql://custom.example.test/conduit
log:
  name: custom-log
"#,
        )?;

        let no_env = |_: &str| None;
        let config =
            load_from_optional_path_with_env(Some(&path), &CliOverrides::default(), &no_env)?;
        assert_eq!(config.server.port, 9001);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.db.dsn, "postgresql://custom.example.test/conduit");
        assert_eq!(config.log.name, "custom-log");
        Ok(())
    }

    #[test]
    fn env_server_port_overrides_yaml() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.yml");
        fs::write(
            &path,
            r#"
server:
  port: 9001
"#,
        )?;

        let env = |key: &str| (key == "CONDUIT_SERVER_PORT").then(|| "8080".to_string());
        let config = load_from_optional_path_with_env(Some(&path), &CliOverrides::default(), &env)?;
        assert_eq!(config.server.port, 8080);
        Ok(())
    }

    #[test]
    fn env_public_url_supports_reverse_proxy_deployments() -> Result<(), Box<dyn std::error::Error>>
    {
        let env = |key: &str| {
            (key == "CONDUIT_SERVER_PUBLIC_URL").then(|| "https://conduit.example.test".to_string())
        };
        let config = load_from_optional_path_with_env(None, &CliOverrides::default(), &env)?;
        assert_eq!(config.server.public_url, "https://conduit.example.test");
        Ok(())
    }

    #[test]
    fn env_disable_ssl_verify_overrides_default() -> Result<(), Box<dyn std::error::Error>> {
        let env =
            |key: &str| (key == "CONDUIT_SERVER_DISABLE_SSL_VERIFY").then(|| "true".to_string());
        let config = load_from_optional_path_with_env(None, &CliOverrides::default(), &env)?;
        assert!(config.server.disable_ssl_verify);
        Ok(())
    }

    #[test]
    fn env_json_like_arrays_parse_json_then_csv() -> Result<(), Box<dyn std::error::Error>> {
        let env = |key: &str| {
            (key == "CONDUIT_PROVIDER_QUOTA_PROVIDERS")
                .then(|| r#"["openai","anthropic"]"#.to_string())
        };
        let config = load_from_optional_path_with_env(None, &CliOverrides::default(), &env)?;
        assert_eq!(config.provider_quota.providers, vec!["openai", "anthropic"]);

        let env = |key: &str| {
            (key == "CONDUIT_PROVIDER_QUOTA_PROVIDERS").then(|| "OpenAI, Anthropic".to_string())
        };
        let config = load_from_optional_path_with_env(None, &CliOverrides::default(), &env)?;
        assert_eq!(config.provider_quota.providers, vec!["OpenAI", "Anthropic"]);
        Ok(())
    }

    #[test]
    fn env_trusted_proxies_accepts_json_array() -> Result<(), Box<dyn std::error::Error>> {
        let env = |key: &str| {
            (key == "CONDUIT_SERVER_TRUSTED_PROXIES")
                .then(|| r#"["127.0.0.1","10.0.0.0/8"]"#.to_string())
        };
        let config = load_from_optional_path_with_env(None, &CliOverrides::default(), &env)?;
        assert_eq!(
            config.server.trusted_proxies,
            vec!["127.0.0.1", "10.0.0.0/8"]
        );
        Ok(())
    }

    #[test]
    fn env_parse_errors_redact_the_rejected_value() {
        const SECRET_VALUE: &str = "not-a-duration-with-secret-token";
        let env =
            |key: &str| (key == "CONDUIT_API_AUTH_SESSION_TTL").then(|| SECRET_VALUE.to_string());
        let error = match load_from_optional_path_with_env(None, &CliOverrides::default(), &env) {
            Err(error) => error,
            Ok(_) => panic!("expected invalid duration to fail"),
        };

        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.contains("CONDUIT_API_AUTH_SESSION_TTL"));
            assert!(!rendered.contains(SECRET_VALUE), "{rendered}");
        }
    }

    #[test]
    fn structured_env_parse_errors_do_not_render_secret_input() {
        const SECRET_VALUE: &str = "oidc-client-secret-review-marker";
        let rejected = format!(r#"{{"client_secret":"{SECRET_VALUE}"}}"#);
        let error = parse_json_or_csv::<OidcProviderConfig>("CONDUIT_OIDC_PROVIDERS", &rejected)
            .expect_err("a JSON object is not a provider array");

        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(rendered.contains("CONDUIT_OIDC_PROVIDERS"));
            assert!(!rendered.contains(SECRET_VALUE), "{rendered}");
            assert!(!rendered.contains("client_secret"), "{rendered}");
        }
    }
}

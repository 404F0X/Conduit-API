use crate::model::{AppConfig, duration_format};
use serde_json::Value;

pub const SECRET_MASK: &str = "********";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    pub key: &'static str,
    pub value: String,
    pub secret: bool,
}

impl EnvEntry {
    fn new(key: &'static str, value: impl Into<String>) -> Self {
        Self {
            key,
            value: value.into(),
            secret: is_secret_key(key),
        }
    }
}

pub fn default_env_entries() -> Vec<EnvEntry> {
    env_entries(&AppConfig::default())
}

pub fn env_entries(config: &AppConfig) -> Vec<EnvEntry> {
    vec![
        EnvEntry::new("CONDUIT_SERVER_NAME", &config.server.name),
        EnvEntry::new("CONDUIT_SERVER_HOST", &config.server.host),
        EnvEntry::new("CONDUIT_SERVER_PUBLIC_URL", &config.server.public_url),
        EnvEntry::new("CONDUIT_SERVER_PORT", config.server.port.to_string()),
        EnvEntry::new("CONDUIT_SERVER_BASE_PATH", &config.server.base_path),
        EnvEntry::new(
            "CONDUIT_SERVER_TRUSTED_PROXIES",
            json_array(&config.server.trusted_proxies),
        ),
        EnvEntry::new("CONDUIT_SERVER_DEBUG", config.server.debug.to_string()),
        EnvEntry::new(
            "CONDUIT_SERVER_DISABLE_SSL_VERIFY",
            config.server.disable_ssl_verify.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_SERVER_CORS_ALLOWED_ORIGINS",
            json_array(&config.server.cors.allowed_origins),
        ),
        EnvEntry::new(
            "CONDUIT_SERVER_READ_TIMEOUT",
            duration_format::format_duration(config.server.read_timeout),
        ),
        EnvEntry::new("CONDUIT_DB_DIALECT", &config.db.dialect),
        EnvEntry::new("CONDUIT_DB_DSN", &config.db.dsn),
        EnvEntry::new("CONDUIT_DB_DEBUG", config.db.debug.to_string()),
        EnvEntry::new(
            "CONDUIT_DB_MAX_OPEN_CONNS",
            config.db.max_open_conns.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_DB_MAX_IDLE_CONNS",
            config.db.max_idle_conns.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_DB_CONN_MAX_LIFETIME",
            duration_format::format_duration(config.db.conn_max_lifetime),
        ),
        EnvEntry::new(
            "CONDUIT_DB_CONN_MAX_IDLE_TIME",
            duration_format::format_duration(config.db.conn_max_idle_time),
        ),
        EnvEntry::new(
            "CONDUIT_DB_DISABLE_AUTO_MIGRATION",
            config.db.disable_auto_migration.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_DB_CONNECT_TIMEOUT",
            duration_format::format_duration(config.db.connect_timeout),
        ),
        EnvEntry::new(
            "CONDUIT_DB_READ_REPLICA_READ_DSN",
            &config.db.read_replica.read_dsn,
        ),
        EnvEntry::new(
            "CONDUIT_DB_READ_REPLICA_READ_MAX_OPEN_CONNS",
            config.db.read_replica.read_max_open_conns.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_DB_READ_REPLICA_READ_MAX_IDLE_CONNS",
            config.db.read_replica.read_max_idle_conns.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_DB_READ_REPLICA_FALLBACK_ON_REPLICA_FAILURE",
            config
                .db
                .read_replica
                .fallback_on_replica_failure
                .to_string(),
        ),
        EnvEntry::new("CONDUIT_LOG_NAME", &config.log.name),
        EnvEntry::new("CONDUIT_LOG_LEVEL", &config.log.level),
        EnvEntry::new("CONDUIT_LOG_FORMAT", &config.log.format),
        EnvEntry::new("CONDUIT_LOG_DIRECTORY", &config.log.directory),
        EnvEntry::new("CONDUIT_LOG_STDOUT", config.log.stdout.to_string()),
        EnvEntry::new("CONDUIT_CACHE_MODE", &config.cache.mode),
        EnvEntry::new(
            "CONDUIT_CACHE_MEMORY_MAX_CAPACITY",
            config.cache.memory.max_capacity.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_MEMORY_TTL",
            duration_format::format_duration(config.cache.memory.ttl),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_ROUTE_AFFINITY_ENABLED",
            config.cache.route_affinity.enabled.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_ROUTE_AFFINITY_PROMPT_CACHE_TTL",
            duration_format::format_duration(config.cache.route_affinity.prompt_cache_ttl),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_ROUTE_AFFINITY_RESPONSE_CONTINUITY_TTL",
            duration_format::format_duration(config.cache.route_affinity.response_continuity_ttl),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_ROUTE_AFFINITY_LOOKUP_CACHE_TTL",
            duration_format::format_duration(config.cache.route_affinity.lookup_cache_ttl),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_ROUTE_AFFINITY_NEGATIVE_CACHE_TTL",
            duration_format::format_duration(config.cache.route_affinity.negative_cache_ttl),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_URL",
            option_string(&config.cache.redis.url),
        ),
        EnvEntry::new("CONDUIT_CACHE_REDIS_ADDR", &config.cache.redis.addr),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_ADDRS",
            json_array(&config.cache.redis.addrs),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_USERNAME",
            option_string(&config.cache.redis.username),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_PASSWORD",
            option_string(&config.cache.redis.password),
        ),
        EnvEntry::new("CONDUIT_CACHE_REDIS_DB", config.cache.redis.db.to_string()),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_MASTER_NAME",
            option_string(&config.cache.redis.master_name),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_SENTINEL",
            config.cache.redis.sentinel.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_TLS",
            config.cache.redis.tls.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_CACHE_REDIS_CLUSTER",
            config.cache.redis.cluster.to_string(),
        ),
        EnvEntry::new(
            "CONDUIT_METRICS_ENABLED",
            config.metrics.enabled.to_string(),
        ),
        EnvEntry::new("CONDUIT_METRICS_HOST", &config.metrics.host),
        EnvEntry::new("CONDUIT_METRICS_PORT", config.metrics.port.to_string()),
        EnvEntry::new("CONDUIT_METRICS_PATH", &config.metrics.path),
        EnvEntry::new(
            "CONDUIT_API_AUTH_JWT_SECRET",
            option_string(&config.api_auth.jwt_secret),
        ),
        EnvEntry::new(
            "CONDUIT_API_AUTH_ALLOW_PASSWORD_SIGNUP",
            config.api_auth.allow_password_signup.to_string(),
        ),
    ]
}

pub fn render_env(config: &AppConfig) -> String {
    render_entries(env_entries(config), false)
}

pub fn render_masked_env(config: &AppConfig) -> String {
    render_entries(env_entries(config), true)
}

pub fn render_default_env() -> String {
    render_entries(default_env_entries(), false)
}

pub fn masked_config_preview(config: &AppConfig) -> Value {
    // AppConfig 实现了 Serialize，序列化在实践上不会失败；这里用回退替代被
    // workspace 禁用的 `.expect()`，失败时退回 null 而非 panic。
    let mut value = serde_json::to_value(config).unwrap_or(Value::Null);
    mask_config_value(&mut value);
    value
}

pub fn render_masked_config_preview(config: &AppConfig) -> String {
    serde_json::to_string_pretty(&masked_config_preview(config)).unwrap_or_default()
}

fn render_entries(entries: Vec<EnvEntry>, mask_secrets: bool) -> String {
    entries
        .into_iter()
        .map(|entry| {
            let value = if mask_secrets && entry.secret && !entry.value.is_empty() {
                SECRET_MASK.to_string()
            } else {
                entry.value
            };
            format!("{}={}", entry.key, format_env_value(&value))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn option_string(value: &Option<String>) -> String {
    value.clone().unwrap_or_default()
}

fn json_array(values: &[String]) -> String {
    // 序列化字符串切片不会失败；失败回退到空数组，避免使用被禁用的 `.expect()`。
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn format_env_value(value: &str) -> String {
    if value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '_' | '-' | '.' | '/' | ':' | '[' | ']' | ',' | '*')
    }) {
        value.to_string()
    } else {
        // 字符串序列化不会失败；失败时退化为原始字符串，避免使用被禁用的 `.expect()`。
        serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
    }
}

fn is_secret_key(key: &str) -> bool {
    key.contains("SECRET") || key.contains("PASSWORD") || key.contains("TOKEN")
}

fn mask_config_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_config_field(key) {
                    mask_present_value(value);
                } else {
                    mask_config_value(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                mask_config_value(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn mask_present_value(value: &mut Value) {
    if !is_empty_config_value(value) {
        *value = Value::String(SECRET_MASK.to_string());
    }
}

fn is_empty_config_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(object) => object.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn is_secret_config_field(key: &str) -> bool {
    key.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|part| matches!(part, "secret" | "key" | "password" | "token"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AppConfig, OidcProviderConfig};

    #[test]
    fn default_env_contains_core_runtime_values() {
        let text = render_default_env();

        assert!(text.contains("CONDUIT_SERVER_PORT=8090"));
        assert!(text.contains("CONDUIT_SERVER_TRUSTED_PROXIES=[]"));
        assert!(text.contains("CONDUIT_SERVER_DISABLE_SSL_VERIFY=false"));
        assert!(text.contains("CONDUIT_DB_DIALECT=postgres"));
        assert!(
            text.contains(
                r#"CONDUIT_DB_DSN="postgresql://conduit:conduit@127.0.0.1:5432/conduit""#
            )
        );
        assert!(text.contains("CONDUIT_DB_MAX_OPEN_CONNS=20"));
        assert!(text.contains("CONDUIT_DB_MAX_IDLE_CONNS=10"));
        assert!(text.contains("CONDUIT_DB_CONN_MAX_LIFETIME=30m"));
        assert!(text.contains("CONDUIT_DB_CONN_MAX_IDLE_TIME=10m"));
        assert!(text.contains("CONDUIT_LOG_LEVEL=info"));
        assert!(text.contains("CONDUIT_CACHE_MODE=memory"));
        assert!(text.contains("CONDUIT_CACHE_ROUTE_AFFINITY_ENABLED=true"));
        assert!(text.contains("CONDUIT_CACHE_ROUTE_AFFINITY_PROMPT_CACHE_TTL=1h"));
        assert!(text.contains("CONDUIT_CACHE_REDIS_ADDR=127.0.0.1:6379"));
        assert!(text.contains("CONDUIT_API_AUTH_ALLOW_PASSWORD_SIGNUP=false"));
    }

    #[test]
    fn env_entries_render_custom_values_and_json_arrays() {
        let mut config = AppConfig::default();
        config.server.public_url = "https://conduit.example.test".to_string();
        config.server.disable_ssl_verify = true;
        config.server.cors.allowed_origins = vec!["https://example.test".to_string()];
        config.cache.redis.addrs = vec![
            "redis-a.example.test:6379".to_string(),
            "redis-b.example.test:6379".to_string(),
        ];
        config.log.level = "debug".to_string();

        let text = render_env(&config);

        assert!(
            text.contains(r#"CONDUIT_SERVER_CORS_ALLOWED_ORIGINS="[\"https://example.test\"]""#)
        );
        assert!(text.contains("CONDUIT_SERVER_PUBLIC_URL=https://conduit.example.test"));
        assert!(text.contains("CONDUIT_SERVER_DISABLE_SSL_VERIFY=true"));
        assert!(text.contains(r#"CONDUIT_CACHE_REDIS_ADDRS="[\"redis-a.example.test:6379\",\"redis-b.example.test:6379\"]""#));
        assert!(text.contains("CONDUIT_LOG_LEVEL=debug"));
    }

    #[test]
    fn masked_env_hides_secret_values_without_hiding_empty_defaults() {
        let mut config = AppConfig::default();
        config.cache.redis.password = Some("redis-password".to_string());
        config.api_auth.jwt_secret = Some("jwt-secret".to_string());

        let text = render_masked_env(&config);

        assert!(text.contains("CONDUIT_CACHE_REDIS_PASSWORD=********"));
        assert!(text.contains("CONDUIT_API_AUTH_JWT_SECRET=********"));
        assert!(!text.contains("redis-password"));
        assert!(!text.contains("jwt-secret"));
    }

    #[test]
    fn masked_config_preview_masks_nested_secret_key_password_token_fields() {
        let mut config = AppConfig::default();
        config.cache.redis.password = Some("redis-password".to_string());
        config.api_auth.jwt_secret = Some("jwt-secret".to_string());
        config.oidc.providers = vec![OidcProviderConfig {
            name: "oidc".to_string(),
            issuer_url: "https://issuer.example.test".to_string(),
            client_id: "client-id".to_string(),
            client_secret: "client-secret".to_string(),
            scopes: vec!["openid".to_string()],
            allow_signup: false,
        }];

        let preview = masked_config_preview(&config);

        assert_eq!(preview["cache"]["redis"]["password"], SECRET_MASK);
        assert_eq!(preview["api_auth"]["jwt_secret"], SECRET_MASK);
        assert_eq!(
            preview["oidc"]["providers"][0]["client_secret"],
            SECRET_MASK
        );
        assert_eq!(
            preview["oidc"]["providers"][0]["issuer_url"],
            "https://issuer.example.test"
        );
        assert_eq!(preview["server"]["name"], "conduit");
        assert_eq!(preview["cache"]["redis"]["addr"], "127.0.0.1:6379");
    }

    #[test]
    fn masked_config_preview_leaves_empty_secret_fields_empty() {
        let mut config = AppConfig::default();
        config.cache.redis.password = Some(String::new());
        config.api_auth.jwt_secret = None;
        config.oidc.providers = vec![OidcProviderConfig::default()];

        let preview = masked_config_preview(&config);

        assert_eq!(preview["cache"]["redis"]["password"], "");
        assert!(preview["api_auth"]["jwt_secret"].is_null());
        assert_eq!(preview["oidc"]["providers"][0]["client_secret"], "");
    }
}

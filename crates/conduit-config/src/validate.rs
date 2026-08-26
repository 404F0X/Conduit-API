use crate::model::AppConfig;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    errors: Vec<String>,
}

impl ValidationError {
    pub fn errors(&self) -> &[String] {
        &self.errors
    }
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "config validation failed: {}", self.errors.join("; "))
    }
}

impl Error for ValidationError {}

pub fn validate(config: &AppConfig) -> Result<(), ValidationError> {
    let mut errors = Vec::new();

    validate_port("server.port", config.server.port, &mut errors);
    validate_port("metrics.port", config.metrics.port, &mut errors);

    if config.server.host.trim().is_empty() {
        errors.push("server.host must not be empty".to_string());
    }

    let database_dialect = config.db.dialect.trim();
    if database_dialect.is_empty() {
        errors.push("db.dialect must not be empty".to_string());
    } else if !matches!(
        database_dialect.to_ascii_lowercase().as_str(),
        "postgres" | "postgresql" | "pg"
    ) {
        errors.push(format!(
            "db.dialect {database_dialect:?} is unsupported; current release supports PostgreSQL only (accepted values: postgres, postgresql, pg)"
        ));
    }

    if config.db.dsn.trim().is_empty() {
        errors.push("db.dsn must not be empty".to_string());
    } else {
        validate_postgres_dsn("db.dsn", &config.db.dsn, &mut errors);
    }

    if config.log.name.trim().is_empty() {
        errors.push("log.name must not be empty".to_string());
    }

    if config.log.level.trim().is_empty() {
        errors.push("log.level must not be empty".to_string());
    }

    validate_base_path(&config.server.base_path, &mut errors);
    validate_cors_origins(&config.server.cors.allowed_origins, &mut errors);

    if config.retry.timeout > Duration::from_secs(600) {
        errors.push("retry.timeout must not exceed 600s".to_string());
    }

    if config.cache.route_affinity.enabled {
        for (name, value) in [
            (
                "cache.route_affinity.prompt_cache_ttl",
                config.cache.route_affinity.prompt_cache_ttl,
            ),
            (
                "cache.route_affinity.response_continuity_ttl",
                config.cache.route_affinity.response_continuity_ttl,
            ),
            (
                "cache.route_affinity.lookup_cache_ttl",
                config.cache.route_affinity.lookup_cache_ttl,
            ),
            (
                "cache.route_affinity.negative_cache_ttl",
                config.cache.route_affinity.negative_cache_ttl,
            ),
        ] {
            if value.is_zero() {
                errors.push(format!("{name} must be greater than zero when enabled"));
            }
        }
    }

    if !(0.0..=1.0).contains(&config.provider_quota.warning_ratio) {
        errors.push("provider_quota.warning_ratio must be between 0 and 1".to_string());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError { errors })
    }
}

fn validate_port(name: &str, port: u16, errors: &mut Vec<String>) {
    if port == 0 {
        errors.push(format!("{name} must be in 1..=65535"));
    }
}

fn validate_postgres_dsn(name: &str, dsn: &str, errors: &mut Vec<String>) {
    let parsed = match url::Url::parse(dsn.trim()) {
        Ok(parsed) => parsed,
        Err(error) => {
            errors.push(format!("{name} must be a valid PostgreSQL URL: {error}"));
            return;
        }
    };

    if !matches!(parsed.scheme(), "postgres" | "postgresql") {
        errors.push(format!(
            "{name} scheme {:?} is unsupported; current release supports PostgreSQL DSNs only (postgres:// or postgresql://)",
            parsed.scheme()
        ));
    }
}

fn validate_base_path(base_path: &str, errors: &mut Vec<String>) {
    if base_path.is_empty() {
        return;
    }
    if !base_path.starts_with('/') {
        errors.push("server.base_path must be empty or start with '/'".to_string());
    }
    if base_path.len() > 1 && base_path.ends_with('/') {
        errors.push("server.base_path must not end with '/'".to_string());
    }
    if base_path.contains('?') || base_path.contains('#') {
        errors.push("server.base_path must not contain query or fragment".to_string());
    }
    if let Err(message) = validate_base_path_strict(base_path) {
        errors.push(message);
    }
}

/// Pure, standalone `server.base_path` validator.
///
/// Mirrors the Go semantics encoded in the accumulation helper above and the
/// intent of TODO RUST-P1-003 S11/S16: a non-empty base path must start with
/// `/`, must not end with `/` (except for the root `/`), must not carry a query
/// or fragment, and must be URL-safe (unreserved characters plus `/`). Returns
/// the first failure as an owned `String` so callers (e.g. `validate_base_path`
/// in `conduit-http`, or unit tests) can surface it without constructing a full
/// `AppConfig`.
///
/// Note: the Go source (`internal/server/config.go`) declares `BasePath` and
/// defaults it to `""` but never actually wires it into route mounting, so
/// there are no Go golden cases for these rules — the contract is encoded here.
pub fn validate_base_path_strict(base_path: &str) -> Result<(), String> {
    if base_path.is_empty() {
        return Ok(());
    }
    if !base_path.starts_with('/') {
        return Err("server.base_path must be empty or start with '/'".to_string());
    }
    if base_path.len() > 1 && base_path.ends_with('/') {
        return Err("server.base_path must not end with '/'".to_string());
    }
    if base_path.contains('?') || base_path.contains('#') {
        return Err("server.base_path must not contain query or fragment".to_string());
    }
    for ch in base_path.chars() {
        let safe = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~' | '/');
        if !safe {
            return Err(format!(
                "server.base_path must be URL-safe (unexpected character {ch:?})"
            ));
        }
    }
    Ok(())
}

fn validate_cors_origins(origins: &[String], errors: &mut Vec<String>) {
    for origin in origins {
        let trimmed = origin.trim();
        if trimmed == "*" {
            continue;
        }
        let valid_scheme = trimmed.starts_with("http://") || trimmed.starts_with("https://");
        if trimmed.is_empty() || !valid_scheme {
            errors.push(format!(
                "server.cors.allowed_origins entry {origin:?} must be '*' or an http(s) origin"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AppConfig;

    #[test]
    fn validate_accepts_defaults() -> Result<(), ValidationError> {
        validate(&AppConfig::default())?;
        Ok(())
    }

    #[test]
    fn validate_rejects_required_empty_fields_and_timeout() {
        let mut config = AppConfig::default();
        config.server.port = 0;
        config.server.host.clear();
        config.db.dialect.clear();
        config.db.dsn.clear();
        config.log.name.clear();
        config.log.level.clear();
        config.retry.timeout = Duration::from_secs(601);
        config.server.cors.allowed_origins = vec!["ftp://example.com".to_string()];
        config.cache.route_affinity.prompt_cache_ttl = Duration::ZERO;

        let err = match validate(&config) {
            Err(e) => e,
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        let joined = err.errors().join("\n");
        assert!(joined.contains("server.port"));
        assert!(joined.contains("server.host"));
        assert!(joined.contains("db.dialect"));
        assert!(joined.contains("db.dsn"));
        assert!(joined.contains("log.name"));
        assert!(joined.contains("log.level"));
        assert!(joined.contains("retry.timeout"));
        assert!(joined.contains("server.cors.allowed_origins"));
        assert!(joined.contains("cache.route_affinity.prompt_cache_ttl"));
    }

    #[test]
    fn validate_accepts_postgres_dialect_aliases() -> Result<(), ValidationError> {
        for dialect in ["postgres", "postgresql", "pg", "POSTGRES"] {
            let mut config = AppConfig::default();
            config.db.dialect = dialect.to_string();
            validate(&config)?;
        }
        Ok(())
    }

    #[test]
    fn validate_rejects_non_postgres_dialects_explicitly() {
        for dialect in [
            "sqlite", "sqlite3", "libsql", "turso", "mysql", "mariadb", "tidb",
        ] {
            let mut config = AppConfig::default();
            config.db.dialect = dialect.to_string();

            let err = match validate(&config) {
                Err(error) => error,
                Ok(()) => panic!("expected {dialect} to be rejected"),
            };
            let rendered = err.to_string();
            assert!(
                rendered.contains("supports PostgreSQL only"),
                "dialect={dialect} error={rendered}"
            );
        }
    }

    #[test]
    fn validate_rejects_non_postgres_primary_dsns() {
        for dsn in [
            "sqlite://conduit.db",
            "file:conduit.db",
            "mysql://conduit@127.0.0.1/conduit",
        ] {
            let mut config = AppConfig::default();
            config.db.dsn = dsn.to_string();

            let err = match validate(&config) {
                Err(error) => error,
                Ok(()) => panic!("expected {dsn} to be rejected"),
            };
            let rendered = err.to_string();
            assert!(
                rendered.contains("supports PostgreSQL DSNs only"),
                "dsn={dsn} error={rendered}"
            );
        }
    }

    #[test]
    fn validate_accepts_both_postgres_dsn_schemes() -> Result<(), ValidationError> {
        for dsn in [
            "postgres://conduit:conduit@127.0.0.1:5432/conduit",
            "postgresql://conduit:conduit@127.0.0.1:5432/conduit",
        ] {
            let mut config = AppConfig::default();
            config.db.dsn = dsn.to_string();
            validate(&config)?;
        }
        Ok(())
    }

    #[test]
    fn validate_accepts_empty_base_path() -> Result<(), ValidationError> {
        let mut config = AppConfig::default();
        config.server.base_path = String::new();

        validate(&config)?;
        Ok(())
    }

    #[test]
    fn validate_accepts_base_path_with_leading_slash() -> Result<(), ValidationError> {
        let mut config = AppConfig::default();
        config.server.base_path = "/api".to_string();

        validate(&config)?;
        Ok(())
    }

    #[test]
    fn validate_rejects_base_path_without_leading_slash() {
        let mut config = AppConfig::default();
        config.server.base_path = "api".to_string();

        let err = match validate(&config) {
            Err(e) => e,
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        assert!(err.errors().iter().any(|error| error.contains("start")));
    }

    #[test]
    fn validate_rejects_base_path_with_trailing_slash() {
        let mut config = AppConfig::default();
        config.server.base_path = "/api/".to_string();

        let err = match validate(&config) {
            Err(e) => e,
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        assert!(err.errors().iter().any(|error| error.contains("end")));
    }

    #[test]
    fn validate_rejects_base_path_with_query() {
        let mut config = AppConfig::default();
        config.server.base_path = "/api?x=1".to_string();

        let err = match validate(&config) {
            Err(e) => e,
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        assert!(
            err.errors()
                .iter()
                .any(|error| error.contains("query or fragment"))
        );
    }

    #[test]
    fn validate_base_path_strict_accepts_empty_root_and_unreserved() {
        assert!(validate_base_path_strict("").is_ok());
        assert!(validate_base_path_strict("/").is_ok());
        assert!(validate_base_path_strict("/api").is_ok());
        assert!(validate_base_path_strict("/a-b.c_d~e/inner").is_ok());
    }

    #[test]
    fn validate_base_path_strict_rejects_missing_leading_slash() {
        let err = match validate_base_path_strict("api") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection without leading '/'"),
        };
        assert!(err.contains("start"), "{err}");
    }

    #[test]
    fn validate_base_path_strict_rejects_trailing_slash_except_root() {
        let err = match validate_base_path_strict("/api/") {
            Err(message) => message,
            Ok(()) => panic!("expected rejection of trailing '/'"),
        };
        assert!(err.contains("end"), "{err}");
        // Root is the only value permitted to end with '/'.
        assert!(validate_base_path_strict("/").is_ok());
    }

    #[test]
    fn validate_base_path_strict_rejects_query_fragment_and_unsafe_chars() {
        for (value, needle) in [
            ("/api?x=1", "query or fragment"),
            ("/api#frag", "query or fragment"),
            ("/api path", "URL-safe"),
            ("/café", "URL-safe"),
            ("/api;inject", "URL-safe"),
        ] {
            let err = match validate_base_path_strict(value) {
                Err(message) => message,
                Ok(()) => panic!("expected rejection for {value:?}"),
            };
            assert!(err.contains(needle), "value={value:?} err={err}");
        }
    }

    #[test]
    fn validate_surfaces_base_path_url_safety_errors_via_accumulator() {
        // Confirms the accumulation helper `validate_base_path` still pushes the
        // strict-check messages into the shared error Vec consumed by
        // `validate(&AppConfig)`.
        let mut config = AppConfig::default();
        config.server.base_path = "/api space".to_string();

        let err = match validate(&config) {
            Err(e) => e,
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        assert!(
            err.errors().iter().any(|error| error.contains("URL-safe")),
            "{}",
            err
        );
    }
}

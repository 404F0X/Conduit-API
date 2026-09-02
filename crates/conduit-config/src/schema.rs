use crate::model::AppConfig;
use schemars::schema_for;
use std::fs;
use std::io;
use std::path::Path;

pub fn generate_schema_value() -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(schema_for!(AppConfig))
}

pub fn generate_schema_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&generate_schema_value()?)
}

pub fn write_schema(path: impl AsRef<Path>) -> io::Result<()> {
    fs::write(path, generate_schema_json().map_err(io::Error::other)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AppConfig;
    use crate::validate::validate;

    #[test]
    fn schema_contains_top_level_sections() -> Result<(), Box<dyn std::error::Error>> {
        let schema = generate_schema_value()?;
        let definitions = schema
            .get("definitions")
            .and_then(|value| value.as_object())
            .ok_or_else(|| "schema should include definitions".to_string())?;
        assert!(definitions.contains_key("ServerConfig"));
        assert!(definitions.contains_key("DatabaseConfig"));
        assert!(definitions.contains_key("ApiAuthConfig"));
        Ok(())
    }

    #[test]
    fn schema_exposes_defaulted_core_config_fields() -> Result<(), Box<dyn std::error::Error>> {
        let schema = generate_schema_value()?;
        let properties = schema
            .get("properties")
            .and_then(|value| value.as_object())
            .ok_or_else(|| "schema should include top-level properties".to_string())?;

        for key in [
            "server",
            "db",
            "log",
            "metrics",
            "cache",
            "gc",
            "provider_quota",
            "oidc",
            "api_auth",
            "retry",
        ] {
            assert!(properties.contains_key(key), "missing schema key {key}");
        }

        assert_eq!(
            schema
                .get("additionalProperties")
                .and_then(|value| value.as_bool()),
            Some(false)
        );
        Ok(())
    }

    #[test]
    fn schema_generation_helpers_are_deterministic_and_expose_core_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let first = generate_schema_json()?;
        let second = generate_schema_json()?;
        assert_eq!(first, second, "schema JSON generation should be stable");

        let parsed: serde_json::Value = serde_json::from_str(&first)?;
        assert_eq!(parsed, generate_schema_value()?);

        let tempdir = tempfile::tempdir()?;
        let schema_path = tempdir.path().join("config.schema.json");
        write_schema(&schema_path)?;

        let written = fs::read_to_string(schema_path)?;
        assert_eq!(written, first);

        let definitions = parsed
            .get("definitions")
            .and_then(|value| value.as_object())
            .ok_or_else(|| "schema should include definitions".to_string())?;

        for (definition, fields) in [
            ("ServerConfig", &["host", "port", "base_path"][..]),
            (
                "DatabaseConfig",
                &[
                    "dialect",
                    "dsn",
                    "max_open_conns",
                    "conn_max_lifetime",
                    "read_replica",
                ][..],
            ),
            (
                "CacheConfig",
                &["mode", "memory", "redis", "route_affinity"][..],
            ),
            ("MemoryCacheConfig", &["max_capacity", "ttl"][..]),
            (
                "RouteAffinityConfig",
                &[
                    "enabled",
                    "prompt_cache_ttl",
                    "response_continuity_ttl",
                    "lookup_cache_ttl",
                    "negative_cache_ttl",
                ][..],
            ),
            (
                "RedisConfig",
                &[
                    "url",
                    "addr",
                    "addrs",
                    "username",
                    "password",
                    "db",
                    "master_name",
                    "sentinel",
                    "tls",
                    "cluster",
                    "connect_timeout",
                ][..],
            ),
            (
                "GcConfig",
                &[
                    "enabled",
                    "stale_processing_enabled",
                    "stale_processing_interval",
                    "requests_cleanup_enabled",
                    "requests_retention",
                    "usage_logs_cleanup_enabled",
                    "usage_logs_retention",
                ][..],
            ),
            (
                "ProviderQuotaConfig",
                &["enabled", "check_interval", "warning_ratio", "providers"][..],
            ),
            (
                "OidcConfig",
                &["enabled", "redirect_base_url", "state_ttl", "providers"][..],
            ),
            (
                "OidcProviderConfig",
                &[
                    "name",
                    "issuer_url",
                    "client_id",
                    "client_secret",
                    "scopes",
                    "allow_signup",
                ][..],
            ),
            (
                "ApiAuthConfig",
                &[
                    "jwt_secret",
                    "session_ttl",
                    "bcrypt_cost",
                    "allow_password_signup",
                ][..],
            ),
            (
                "RetryConfig",
                &["enabled", "max_channel_retries", "timeout"][..],
            ),
        ] {
            let properties = definitions
                .get(definition)
                .and_then(|value| value.get("properties"))
                .and_then(|value| value.as_object())
                .unwrap_or_else(|| panic!("{definition} should expose properties"));

            for field in fields {
                assert!(
                    properties.contains_key(*field),
                    "{definition} schema missing field {field}"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn defaults_round_trip_and_validate() -> Result<(), Box<dyn std::error::Error>> {
        let config = AppConfig::default();
        let encoded = serde_json::to_value(&config)?;
        let decoded: AppConfig = serde_json::from_value(encoded)?;

        assert_eq!(decoded, config);
        validate(&decoded)?;
        Ok(())
    }

    #[test]
    fn missing_fields_default_but_illegal_values_are_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let partial: AppConfig = serde_yaml::from_str(
            r#"
server:
  port: 9000
"#,
        )?;

        assert_eq!(partial.server.port, 9000);
        assert_eq!(partial.db.dialect, "postgres");
        validate(&partial)?;

        let invalid: AppConfig = serde_yaml::from_str(
            r#"
server:
  host: ""
provider_quota:
  warning_ratio: 1.5
"#,
        )?;

        let errors = match validate(&invalid) {
            Err(e) => e.errors().join("\n"),
            Ok(()) => panic!("expected validation error, got Ok"),
        };
        assert!(errors.contains("server.host"));
        assert!(errors.contains("provider_quota.warning_ratio"));

        let unknown = serde_yaml::from_str::<AppConfig>(
            r#"
server:
  unknown_field: true
"#,
        );
        match unknown {
            Err(err) => assert!(err.to_string().contains("unknown_field")),
            Ok(_) => panic!("unknown fields should be rejected by the model"),
        }
        Ok(())
    }
}

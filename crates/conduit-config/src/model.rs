use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const NO_AUTH_SENTINEL: &str = "CONDUIT_API_KEY_NO_AUTH";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub db: DatabaseConfig,
    pub log: LogConfig,
    pub metrics: MetricsConfig,
    pub cache: CacheConfig,
    pub gc: GcConfig,
    pub provider_quota: ProviderQuotaConfig,
    pub oidc: OidcConfig,
    pub api_auth: ApiAuthConfig,
    pub retry: RetryConfig,
}

// Mirrors Go `server.Config` (`conduit/internal/server/config.go`) field
// set, yaml/json tags, and defaults (sourced from `conduit/conf/conf.go`
// `setDefaults`). Divergences are documented inline:
//   * `write_timeout` and `graceful_shutdown_timeout` have NO Go counterpart
//     at this struct (Go's HTTP server sets these via the `http.Server` ctor,
//     not via config). They are kept because the Rust `axum`/hyper runtime
//     consumes them directly from this struct (see `conduit-http`).
//   * `CorsConfig` is a Rust-authored shape that predates this parity pass;
//     Go `server.CORS` additionally has `enabled`, `debug`, `exposed_headers`,
//     `max_age` (and the default allowed_origins/methods/headers differ).
//     Aligning CORS is tracked separately (out of scope for P1-003 S12).
//
// Fields new in S12 (PublicURL, RequestTimeout, LLMRequestTimeout, Trace,
// Dashboard, DisableSSLVerify, API) mirror Go 1:1. None of them are `omitempty`
// in Go, so none get `skip_serializing_if` here; all carry `#[serde(default)]`
// via the container attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    pub host: String,
    /// Go `server.Config.PublicURL` (`public_url`). Go default: "".
    pub public_url: String,
    pub port: u16,
    pub base_path: String,
    /// Go `server.Config.RequestTimeout` (`request_timeout`). Go default: 30s.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub request_timeout: Duration,
    /// Go `server.Config.LLMRequestTimeout` (`llm_request_timeout`). Go default: 600s.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub llm_request_timeout: Duration,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub read_timeout: Duration,
    /// NOT WIRED (P-53): Rust-only field, parsed but no HTTP server honors it
    /// (axum/hyper have no per-write deadline knob consumed here).
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub write_timeout: Duration,
    /// Rust-only maximum drain window after an interrupt/terminate signal.
    /// The binary passes it to the bounded axum graceful-shutdown path.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub graceful_shutdown_timeout: Duration,
    /// Go `server.Config.Trace` (`trace`) — mirrors `tracing.Config`
    /// (`conduit/internal/tracing/tracing.go`).
    pub trace: TraceConfig,
    /// Go `server.Config.Dashboard` (`dashboard`) — mirrors `server.Dashboard`
    /// (`conduit/internal/server/config.go`). Cache TTLs for dashboard stats.
    pub dashboard: DashboardConfig,
    pub debug: bool,
    /// Go `server.Config.DisableSSLVerify` (`disable_ssl_verify`). Go default: false.
    ///
    /// NOT WIRED (P-53): parsed but no HTTP client honors it. The upstream
    /// `reqwest::Client` instances are built with default TLS verification and
    /// never read this flag, so setting `true` does not relax cert checks (e.g.
    /// for a self-signed upstream). Wiring requires threading the flag into
    /// every `reqwest::Client::builder()` call site (`danger_accept_invalid_certs`).
    pub disable_ssl_verify: bool,
    pub cors: CorsConfig,
    /// Go `server.Config.API` (`api`) — wraps `APIAuth`.
    pub api: ServerApiConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "conduit".to_string(),
            host: "127.0.0.1".to_string(),
            // Go conf.go setDefault: `server.public_url` -> "".
            public_url: String::new(),
            port: 8090,
            base_path: String::new(),
            // Go conf.go setDefault: `server.request_timeout` -> "30s".
            request_timeout: Duration::from_secs(30),
            // Go conf.go setDefault: `server.llm_request_timeout` -> "600s".
            llm_request_timeout: Duration::from_secs(600),
            read_timeout: Duration::from_secs(30),
            // Rust-only (no Go counterpart at this struct).
            write_timeout: Duration::from_secs(30),
            // Rust-only (no Go counterpart at this struct).
            graceful_shutdown_timeout: Duration::from_secs(10),
            trace: TraceConfig::default(),
            dashboard: DashboardConfig::default(),
            debug: false,
            // Go conf.go setDefault: `server.disable_ssl_verify` -> false.
            disable_ssl_verify: false,
            cors: CorsConfig::default(),
            api: ServerApiConfig::default(),
        }
    }
}

/// Mirrors Go `tracing.Config` (`conduit/internal/tracing/tracing.go`).
/// All fields are non-pointer in Go (zero value = Go default), so all are
/// `#[serde(default)]`-filled here with the Go `conf.go` setDefaults values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct TraceConfig {
    /// Go `tracing.Config.ThreadHeader`. Go default: "Conduit-Thread-Id".
    pub thread_header: String,
    /// Go `tracing.Config.TraceHeader`. Go default: "Conduit-Trace-Id".
    pub trace_header: String,
    /// Go `tracing.Config.RequestHeader`. Go has no setDefault for this; the
    /// Go zero value is "". (Code that reads it falls back to "Conduit-Request-Id".)
    pub request_header: String,
    /// Go `tracing.Config.ExtraTraceHeaders`. Go default: [].
    pub extra_trace_headers: Vec<String>,
    /// Go `tracing.Config.ExtraTraceBodyFields`. Go default: [].
    pub extra_trace_body_fields: Vec<String>,
    /// Go `tracing.Config.ClaudeCodeTraceEnabled`. Go default: false.
    pub claude_code_trace_enabled: bool,
    /// Go `tracing.Config.CodexTraceEnabled`. Go default: false.
    pub codex_trace_enabled: bool,
    /// Go `tracing.Config.OpenCodeTraceEnabled`. Go default: false.
    pub opencode_trace_enabled: bool,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            // Go conf.go setDefault: `server.trace.thread_header` -> "Conduit-Thread-Id".
            thread_header: "Conduit-Thread-Id".to_string(),
            // Go conf.go setDefault: `server.trace.trace_header` -> "Conduit-Trace-Id".
            trace_header: "Conduit-Trace-Id".to_string(),
            // No Go setDefault; zero value "" (runtime falls back to "Conduit-Request-Id").
            request_header: String::new(),
            // Go conf.go setDefault: `server.trace.extra_trace_headers` -> [].
            extra_trace_headers: Vec::new(),
            // Go conf.go setDefault: `server.trace.extra_trace_body_fields` -> [].
            extra_trace_body_fields: Vec::new(),
            // Go conf.go setDefault: `server.trace.claude_code_trace_enabled` -> false.
            claude_code_trace_enabled: false,
            // Go conf.go setDefault: `server.trace.codex_trace_enabled` -> false.
            codex_trace_enabled: false,
            // Go conf.go setDefault: `server.trace.opencode_trace_enabled` -> false.
            opencode_trace_enabled: false,
        }
    }
}

/// Mirrors Go `server.Dashboard` (`conduit/internal/server/config.go`).
/// Stale-while-revalidate TTLs for cached all-time token stats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DashboardConfig {
    /// Go `Dashboard.AllTimeTokenStatsSoftTTL` (`all_time_token_stats_soft_ttl`).
    /// Go default: 1h.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub all_time_token_stats_soft_ttl: Duration,
    /// Go `Dashboard.AllTimeTokenStatsHardTTL` (`all_time_token_stats_hard_ttl`).
    /// Go default: 24h.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub all_time_token_stats_hard_ttl: Duration,
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            // Go conf.go setDefault: `server.dashboard.all_time_token_stats_soft_ttl` -> "1h".
            all_time_token_stats_soft_ttl: Duration::from_secs(60 * 60),
            // Go conf.go setDefault: `server.dashboard.all_time_token_stats_hard_ttl` -> "24h".
            all_time_token_stats_hard_ttl: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Mirrors Go `server.API` (`conduit/internal/server/config.go`) — a thin
/// wrapper whose only field is `auth: APIAuth`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct ServerApiConfig {
    /// Go `API.Auth` (`auth`) — mirrors `server.APIAuth`.
    pub auth: ServerApiAuthConfig,
}

/// Mirrors Go `server.APIAuth` (`conduit/internal/server/config.go`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ServerApiAuthConfig {
    /// Go `APIAuth.AllowNoAuth` (`allow_no_auth`). Go default: false.
    pub allow_no_auth: bool,
    /// Go `APIAuth.KeyPrefix` (`key_prefix`). Go default: "conduit".
    pub key_prefix: String,
}

impl Default for ServerApiAuthConfig {
    fn default() -> Self {
        Self {
            // Go conf.go setDefault: `server.api.auth.allow_no_auth` -> false.
            allow_no_auth: false,
            // Go conf.go setDefault: `server.api.auth.key_prefix` -> "conduit".
            key_prefix: "conduit".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allowed_headers: Vec<String>,
    pub allow_credentials: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_methods: vec![
                "GET".to_string(),
                "POST".to_string(),
                "PUT".to_string(),
                "PATCH".to_string(),
                "DELETE".to_string(),
                "OPTIONS".to_string(),
            ],
            allowed_headers: vec!["authorization".to_string(), "content-type".to_string()],
            allow_credentials: true,
        }
    }
}

// Field names remain compatible with Conduit API's Go `db.Config`
// (`conduit/internal/server/db/config.go`). The active Rust product is
// PostgreSQL-only, so its database defaults intentionally differ from the
// legacy Go contract snapshot.
//
// Go `database/sql` semantics differ from sqlx (see `conduit-db::pool`):
// Go treats a `0` for `max_open_conns`/`max_idle_conns` as *unlimited*; the
// Rust runtime pool (Bohr's `conduit-db::DatabaseConfig`) coalesces a `0` to
// the configured default. This config struct keeps the raw configured value;
// runtime normalization happens in the db crate. The two `DatabaseConfig`
// types are intentionally separate: this one is the serde/schema contract,
// the db-crate one is the sqlx runtime shape used to open PostgreSQL pools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    pub dialect: String,
    pub dsn: String,
    pub debug: bool,
    /// Maps to Go `db.max_open_conns` (`SetMaxOpenConns`). Go default 20.
    pub max_open_conns: u32,
    /// Maps to Go `db.max_idle_conns` (`SetMaxIdleConns`). Go default 10.
    pub max_idle_conns: u32,
    /// Maps to Go `db.conn_max_lifetime` (`SetConnMaxLifetime`). Go default "30m".
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub conn_max_lifetime: Duration,
    /// Maps to Go `db.conn_max_idle_time` (`SetConnMaxIdleTime`). Go default "10m".
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub conn_max_idle_time: Duration,
    pub disable_auto_migration: bool,
    /// Acquire timeout for a connection from the pool (sqlx-side; no direct Go
    /// `db.Config` equivalent). Kept for runtime use. Default 30s.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub connect_timeout: Duration,
    pub read_replica: DbReadReplicaConfig,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            dialect: "postgres".to_string(),
            dsn: "postgresql://conduit:conduit@127.0.0.1:5432/conduit".to_string(),
            debug: false,
            max_open_conns: 20,
            max_idle_conns: 10,
            conn_max_lifetime: Duration::from_secs(30 * 60),
            conn_max_idle_time: Duration::from_secs(10 * 60),
            disable_auto_migration: false,
            connect_timeout: Duration::from_secs(30),
            read_replica: DbReadReplicaConfig::default(),
        }
    }
}

/// Mirrors Go `db.ReadReplicaConfig` (`conduit/internal/server/db/config.go`)
/// plus the Rust-only `fallback_on_replica_failure` switch (RUST-P3-001 S11).
/// Defaults: empty DSN, 0 conns (=> use pool defaults / disabled),
/// `fallback_on_replica_failure = false`.
///
/// Go semantics (verified against `conduit/internal/server/db/router.go`):
/// when a replica is configured, the Ent read path calls `replica.Query`
/// *directly* with **no try/catch and no runtime fallback** — a replica
/// failure propagates to the caller. To stay byte-compatible with that
/// behavior the Rust default is therefore `false` (do not fall back to master
/// when the replica fails at runtime). Writes (`Exec`) and transactions always
/// run on master regardless of this flag, so replica outages never break the
/// write path — this is what "default compatible with Go" guarantees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct DbReadReplicaConfig {
    pub read_dsn: String,
    pub read_max_open_conns: u32,
    pub read_max_idle_conns: u32,
    /// When `true`, a runtime failure on the replica (connection error,
    /// pool exhaustion, etc.) falls back to executing the read on master.
    /// When `false` (default, Go-compatible) the error is propagated as-is.
    /// Has no effect when no replica is configured (reads always use master)
    /// and never affects writes or transactions.
    pub fallback_on_replica_failure: bool,
}

// Mirrors Go `log.Config` (`conduit/internal/log/logger.go`) field set, yaml
// tags, and defaults (sourced from `conduit/conf/conf.go` `setDefaults`).
//
// Divergences (Rust-only fields kept because downstream crates consume them):
//   * `format` — Go uses `encoding` (json|console|console_json). The Rust side
//     historically has `format` with the same semantics (default "json"). Kept
//     as a Rust convenience alias; aligning to Go's `encoding` rename is
//     tracked separately.
//   * `directory`, `stdout` — Go has no direct equivalent; Go routes output via
//     `output` ("stdio"|"file") + `file.path`. The Rust logging layer still
//     consumes these, so they remain.
// Go fields added in S12: `debug`, `skip_level`, `level_key`, `time_key`,
// `caller_key`, `function_key`, `name_key`, `encoding`, `includes`, `excludes`,
// `output`, `file` (FileConfig).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    /// Go `log.Config.Name` (`name`). Go default: "conduit".
    pub name: String,
    /// Go `log.Config.Debug` (`debug`). Go default: false.
    pub debug: bool,
    /// Go `log.Config.SkipLevel` (`skip_level`). Go default: 1.
    pub skip_level: i32,
    /// Go `log.Config.Level` (`level`). Go accepts a string ("info", "debug",
    /// ...) and parses it to a zapcore.Level. Stored as a string here.
    /// Go default: "info".
    pub level: String,
    /// Go `log.Config.LevelKey` (`level_key`). Go default: "level".
    pub level_key: String,
    /// Go `log.Config.TimeKey` (`time_key`). Go default: "time".
    pub time_key: String,
    /// Go `log.Config.CallerKey` (`caller_key`). Go default: "label".
    pub caller_key: String,
    /// Go `log.Config.FunctionKey` (`function_key`). Go default: "".
    pub function_key: String,
    /// Go `log.Config.NameKey` (`name_key`). Go default: "logger".
    pub name_key: String,
    /// Go `log.Config.Encoding` (`encoding`). Go default: "json".
    pub encoding: String,
    /// Go `log.Config.Includes` (`includes`). Go default: [].
    pub includes: Vec<String>,
    /// Go `log.Config.Excludes` (`excludes`). Go default: [].
    pub excludes: Vec<String>,
    /// Go `log.Config.Output` (`output`). "stdio" or "file". Go default: "stdio".
    pub output: String,
    /// Go `log.Config.File` (`file`) — mirrors `log.FileConfig`.
    pub file: LogFileConfig,

    // --- Rust-only extras (no Go counterpart) — see struct doc comment. ---
    /// Rust-only. Semantic alias for Go `encoding`. Default: "json".
    pub format: String,
    /// Rust-only. Log directory (Go uses `file.path` dirname). Default: "logs".
    pub directory: String,
    /// Rust-only. Mirror of Go `output == "stdio"`. Default: true.
    pub stdout: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            // Go conf.go setDefault: `log.name` -> "conduit".
            name: "conduit".to_string(),
            // Go conf.go setDefault: `log.debug` -> false.
            debug: false,
            // Go conf.go setDefault: `log.skip_level` -> 1.
            skip_level: 1,
            // Go conf.go setDefault: `log.level` -> "info".
            level: "info".to_string(),
            // Go conf.go setDefault: `log.level_key` -> "level".
            level_key: "level".to_string(),
            // Go conf.go setDefault: `log.time_key` -> "time".
            time_key: "time".to_string(),
            // Go conf.go setDefault: `log.caller_key` -> "label".
            caller_key: "label".to_string(),
            // Go conf.go setDefault: `log.function_key` -> "".
            function_key: String::new(),
            // Go conf.go setDefault: `log.name_key` -> "logger".
            name_key: "logger".to_string(),
            // Go conf.go setDefault: `log.encoding` -> "json".
            encoding: "json".to_string(),
            // Go conf.go setDefault: `log.includes` -> [].
            includes: Vec::new(),
            // Go conf.go setDefault: `log.excludes` -> [].
            excludes: Vec::new(),
            // Go conf.go setDefault: `log.output` -> "stdio".
            output: "stdio".to_string(),
            file: LogFileConfig::default(),

            // Rust-only extras.
            format: "json".to_string(),
            directory: "logs".to_string(),
            stdout: true,
        }
    }
}

/// Mirrors Go `log.FileConfig` (`conduit/internal/log/logger.go`) — lumberjack
/// v2 file-rotation options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LogFileConfig {
    /// Go `FileConfig.Path` (`path`). Go default: "logs/conduit.log".
    pub path: String,
    /// Go `FileConfig.MaxSize` (`max_size`) in megabytes. Go default: 100.
    pub max_size: i64,
    /// Go `FileConfig.MaxAge` (`max_age`) in days. Go default: 30.
    pub max_age: i64,
    /// Go `FileConfig.MaxBackups` (`max_backups`) in files. Go default: 10.
    pub max_backups: i64,
    /// Go `FileConfig.LocalTime` (`local_time`). Go default: true.
    pub local_time: bool,
}

impl Default for LogFileConfig {
    fn default() -> Self {
        Self {
            // Go conf.go setDefault: `log.file.path` -> "logs/conduit.log".
            path: "logs/conduit.log".to_string(),
            // Go conf.go setDefault: `log.file.max_size` -> 100.
            max_size: 100,
            // Go conf.go setDefault: `log.file.max_age` -> 30.
            max_age: 30,
            // Go conf.go setDefault: `log.file.max_backups` -> 10.
            max_backups: 10,
            // Go conf.go setDefault: `log.file.local_time` -> true.
            local_time: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            // Off by default, mirroring Go `conf.go:225`
            // (`v.SetDefault("metrics.enabled", false)`) — a metrics endpoint
            // must be opted into, not exposed silently (P-45).
            enabled: false,
            // Loopback by default: even when enabled, the operator must
            // explicitly widen the bind address to expose it off-host. Go uses
            // an OTLP push exporter with no listener; the Rust port keeps a
            // Prometheus pull endpoint but defaults it to a safe posture.
            host: "127.0.0.1".to_string(),
            port: 9090,
            path: "/metrics".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub mode: String,
    pub memory: MemoryCacheConfig,
    pub redis: RedisConfig,
    pub route_affinity: RouteAffinityConfig,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: "memory".to_string(),
            memory: MemoryCacheConfig::default(),
            redis: RedisConfig::default(),
            route_affinity: RouteAffinityConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RouteAffinityConfig {
    pub enabled: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub prompt_cache_ttl: Duration,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub response_continuity_ttl: Duration,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub lookup_cache_ttl: Duration,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub negative_cache_ttl: Duration,
}

impl Default for RouteAffinityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_cache_ttl: Duration::from_secs(60 * 60),
            response_continuity_ttl: Duration::from_secs(7 * 24 * 60 * 60),
            lookup_cache_ttl: Duration::from_secs(60),
            negative_cache_ttl: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryCacheConfig {
    pub max_capacity: u64,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub ttl: Duration,
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self {
            max_capacity: 10_000,
            ttl: Duration::from_secs(300),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RedisConfig {
    pub url: Option<String>,
    pub addr: String,
    pub addrs: Vec<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub db: u8,
    pub master_name: Option<String>,
    pub sentinel: bool,
    pub tls: bool,
    pub cluster: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub connect_timeout: Duration,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: None,
            addr: "127.0.0.1:6379".to_string(),
            addrs: Vec::new(),
            username: None,
            password: None,
            db: 0,
            master_name: None,
            sentinel: false,
            tls: false,
            cluster: false,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GcConfig {
    pub enabled: bool,
    pub stale_processing_enabled: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub stale_processing_interval: Duration,
    pub requests_cleanup_enabled: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub requests_retention: Duration,
    pub usage_logs_cleanup_enabled: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub usage_logs_retention: Duration,
    /// Whether the policy-driven GC runs `VACUUM` after cleanup (Go
    /// `gc.Config.VacuumEnabled`, `gc.go:198`). P-47.
    pub vacuum_enabled: bool,
    /// Whether PostgreSQL VACUUM uses the locking `FULL` variant (Go
    /// `gc.Config.VacuumFull`). P-47.
    pub vacuum_full: bool,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_processing_enabled: true,
            stale_processing_interval: Duration::from_secs(60),
            requests_cleanup_enabled: false,
            requests_retention: Duration::from_secs(3 * 24 * 60 * 60),
            usage_logs_cleanup_enabled: false,
            usage_logs_retention: Duration::from_secs(30 * 24 * 60 * 60),
            vacuum_enabled: false,
            vacuum_full: false,
        }
    }
}

/// Provider quota polling configuration.
///
/// `enabled` and `check_interval` drive the production quota worker. The
/// provider-specific `providers` allowlist is retained for compatibility but
/// is not consumed; all supported checkers are registered. `warning_ratio`
/// currently remains fixed to Go's canonical 0.8 threshold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderQuotaConfig {
    pub enabled: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub check_interval: Duration,
    pub warning_ratio: f64,
    pub providers: Vec<String>,
}

impl Default for ProviderQuotaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            check_interval: Duration::from_secs(10 * 60),
            warning_ratio: 0.8,
            providers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct OidcConfig {
    pub enabled: bool,
    pub redirect_base_url: Option<String>,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub state_ttl: Duration,
    pub providers: Vec<OidcProviderConfig>,
}

impl Default for OidcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            redirect_base_url: None,
            state_ttl: Duration::from_secs(10 * 60),
            providers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct OidcProviderConfig {
    pub name: String,
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scopes: Vec<String>,
    pub allow_signup: bool,
}

impl Default for OidcProviderConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            issuer_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scopes: vec![
                "openid".to_string(),
                "profile".to_string(),
                "email".to_string(),
            ],
            allow_signup: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ApiAuthConfig {
    pub enabled: bool,
    /// NOT WIRED (P-53): parsed but the API-key extractor uses a fixed header
    /// set (Authorization / X-Goog-Api-Key / query param), not this config.
    pub api_key_header: String,
    /// NOT WIRED (P-53): parsed but no auth middleware consumes it; the no-auth
    /// sentinel is always rejected (see `no_auth_sentinel`).
    pub allow_no_auth_fallback: bool,
    /// NOT WIRED (P-53): parsed but the sentinel check uses the hardcoded
    /// `NO_AUTH_SENTINEL` constant (`conduit-auth`), not this config value.
    pub no_auth_sentinel: String,
    /// NOT WIRED (P-53): parsed but no auth middleware consumes it. Admin
    /// access is gated by the JWT resolver (P-01/P-33), not a static token, so
    /// setting this has no effect.
    pub admin_token: Option<String>,
    pub jwt_secret: Option<String>,
    /// NOT WIRED (P-53): parsed but no session store honors it. JWT expiry is
    /// driven by the token `exp` claim at sign time, not this config value.
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub session_ttl: Duration,
    pub bcrypt_cost: u32,
}

impl Default for ApiAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            api_key_header: "Authorization".to_string(),
            allow_no_auth_fallback: false,
            no_auth_sentinel: NO_AUTH_SENTINEL.to_string(),
            admin_token: None,
            jwt_secret: None,
            session_ttl: Duration::from_secs(24 * 60 * 60),
            bcrypt_cost: 12,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RetryConfig {
    pub enabled: bool,
    pub max_channel_retries: u32,
    pub max_single_channel_retries: u32,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub retry_delay: Duration,
    pub strategy: String,
    pub upstream_error_passthrough: bool,
    #[serde(with = "duration_format")]
    #[schemars(with = "String")]
    pub timeout: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay: Duration::from_millis(1_000),
            strategy: "adaptive".to_string(),
            upstream_error_passthrough: true,
            timeout: Duration::from_secs(600),
        }
    }
}

pub mod duration_format {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_duration(*duration))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_duration(&value).map_err(D::Error::custom)
    }

    pub fn parse_duration(input: &str) -> Result<Duration, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("duration must not be empty".to_string());
        }

        let (number, unit) = split_number_unit(trimmed)?;
        match unit {
            "ms" => Ok(Duration::from_millis(number)),
            "s" => Ok(Duration::from_secs(number)),
            "m" => number
                .checked_mul(60)
                .map(Duration::from_secs)
                .ok_or_else(|| format!("duration is too large: {input}")),
            "h" => number
                .checked_mul(60 * 60)
                .map(Duration::from_secs)
                .ok_or_else(|| format!("duration is too large: {input}")),
            "d" => number
                .checked_mul(24 * 60 * 60)
                .map(Duration::from_secs)
                .ok_or_else(|| format!("duration is too large: {input}")),
            _ => Err(format!(
                "unsupported duration unit in {input:?}; expected ms, s, m, h, or d"
            )),
        }
    }

    pub fn format_duration(duration: Duration) -> String {
        let millis = duration.as_millis();
        if millis == 0 {
            return "0s".to_string();
        }

        if millis.is_multiple_of(24 * 60 * 60 * 1_000) {
            return format!("{}d", millis / (24 * 60 * 60 * 1_000));
        }
        if millis.is_multiple_of(60 * 60 * 1_000) {
            return format!("{}h", millis / (60 * 60 * 1_000));
        }
        if millis.is_multiple_of(60 * 1_000) {
            return format!("{}m", millis / (60 * 1_000));
        }
        if millis.is_multiple_of(1_000) {
            return format!("{}s", millis / 1_000);
        }
        format!("{millis}ms")
    }

    fn split_number_unit(input: &str) -> Result<(u64, &str), String> {
        let unit_start = input
            .find(|c: char| !c.is_ascii_digit())
            .ok_or_else(|| format!("duration {input:?} is missing a unit"))?;
        let (number, unit) = input.split_at(unit_start);
        if number.is_empty() {
            return Err(format!("duration {input:?} is missing a numeric value"));
        }
        let number = number
            .parse::<u64>()
            .map_err(|err| format!("invalid duration number {number:?}: {err}"))?;
        Ok((number, unit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn defaults_keep_go_compatible_server_port() {
        let config = AppConfig::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 8090);
        assert_eq!(config.retry.timeout, Duration::from_secs(600));
    }

    /// The Rust product defaults to its only supported database. The legacy
    /// Go defaults remain recorded in `tests/contracts/config_defaults.json`
    /// as historical contract evidence.
    #[test]
    fn database_config_defaults_to_local_postgres() {
        let db = DatabaseConfig::default();

        assert_eq!(db.dialect, "postgres");
        assert_eq!(
            db.dsn,
            "postgresql://conduit:conduit@127.0.0.1:5432/conduit"
        );
        assert!(!db.debug);
        assert_eq!(db.max_open_conns, 20);
        assert_eq!(db.max_idle_conns, 10);
        assert_eq!(
            db.conn_max_lifetime,
            Duration::from_secs(30 * 60),
            "db.conn_max_lifetime default is 30m"
        );
        assert_eq!(
            db.conn_max_idle_time,
            Duration::from_secs(10 * 60),
            "db.conn_max_idle_time default is 10m"
        );
        assert!(!db.disable_auto_migration);
        // connect_timeout is a Rust/sqlx-side pool acquire timeout (30s); Go's
        // db.Config has no direct equivalent.
        assert_eq!(db.connect_timeout, Duration::from_secs(30));

        // read_replica defaults — Go: empty DSN, 0 conns (=> disabled).
        assert_eq!(db.read_replica.read_dsn, "");
        assert_eq!(db.read_replica.read_max_open_conns, 0);
        assert_eq!(db.read_replica.read_max_idle_conns, 0);
        // Rust-only extension (RUST-P3-001 S11); Go-compatible default false.
        assert!(!db.read_replica.fallback_on_replica_failure);
    }

    #[test]
    fn database_config_round_trips_through_serde_json() -> Result<(), serde_json::Error> {
        // Round-trip must preserve all fields, including the duration fields
        // serialized via `duration_format` (e.g. "30m", "10m").
        let original = DatabaseConfig::default();
        let json = serde_json::to_string(&original)?;
        let parsed: DatabaseConfig = serde_json::from_str(&json)?;
        assert_eq!(parsed, original);
        assert!(json.contains("\"conn_max_lifetime\":\"30m\""));
        assert!(json.contains("\"conn_max_idle_time\":\"10m\""));
        Ok(())
    }

    #[test]
    fn database_config_parses_go_shaped_yaml_with_durations() -> Result<(), serde_yaml::Error> {
        let yaml = r#"
dialect: postgres
dsn: postgres://localhost/conduit
max_open_conns: 50
max_idle_conns: 5
conn_max_lifetime: 1h
conn_max_idle_time: 15m
read_replica:
  read_dsn: postgres://replica/conduit
  read_max_open_conns: 10
"#;
        let db: DatabaseConfig = serde_yaml::from_str(yaml)?;
        assert_eq!(db.dialect, "postgres");
        assert_eq!(db.max_open_conns, 50);
        assert_eq!(db.max_idle_conns, 5);
        assert_eq!(db.conn_max_lifetime, Duration::from_secs(60 * 60));
        assert_eq!(db.conn_max_idle_time, Duration::from_secs(15 * 60));
        assert_eq!(db.read_replica.read_dsn, "postgres://replica/conduit");
        assert_eq!(db.read_replica.read_max_open_conns, 10);
        // Fields absent from yaml fall back to defaults.
        assert_eq!(db.read_replica.read_max_idle_conns, 0);
        Ok(())
    }

    #[test]
    fn defaults_include_cache_gc_quota_oidc_and_api_auth_skeletons() {
        let config = AppConfig::default();

        assert_eq!(config.cache.mode, "memory");
        assert_eq!(config.cache.memory.max_capacity, 10_000);
        assert_eq!(config.cache.memory.ttl, Duration::from_secs(300));
        assert!(config.cache.route_affinity.enabled);
        assert_eq!(
            config.cache.route_affinity.prompt_cache_ttl,
            Duration::from_secs(60 * 60)
        );
        assert_eq!(
            config.cache.route_affinity.response_continuity_ttl,
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(
            config.cache.route_affinity.lookup_cache_ttl,
            Duration::from_secs(60)
        );
        assert_eq!(
            config.cache.route_affinity.negative_cache_ttl,
            Duration::from_secs(5)
        );
        assert_eq!(config.cache.redis.url, None);
        assert_eq!(config.cache.redis.addr, "127.0.0.1:6379");
        assert!(config.cache.redis.addrs.is_empty());
        assert_eq!(config.cache.redis.username, None);
        assert_eq!(config.cache.redis.password, None);
        assert_eq!(config.cache.redis.db, 0);
        assert_eq!(config.cache.redis.master_name, None);
        assert!(!config.cache.redis.sentinel);
        assert!(!config.cache.redis.tls);
        assert!(!config.cache.redis.cluster);
        assert_eq!(config.cache.redis.connect_timeout, Duration::from_secs(5));

        assert!(config.gc.enabled);
        assert!(config.gc.stale_processing_enabled);
        assert_eq!(config.gc.stale_processing_interval, Duration::from_secs(60));
        assert!(!config.gc.requests_cleanup_enabled);
        assert_eq!(
            config.gc.requests_retention,
            Duration::from_secs(3 * 24 * 60 * 60)
        );
        assert!(!config.gc.usage_logs_cleanup_enabled);
        assert_eq!(
            config.gc.usage_logs_retention,
            Duration::from_secs(30 * 24 * 60 * 60)
        );

        assert!(config.provider_quota.enabled);
        assert_eq!(
            config.provider_quota.check_interval,
            Duration::from_secs(10 * 60)
        );
        assert_eq!(config.provider_quota.warning_ratio, 0.8);
        assert!(config.provider_quota.providers.is_empty());

        assert!(!config.oidc.enabled);
        assert_eq!(config.oidc.redirect_base_url, None);
        assert_eq!(config.oidc.state_ttl, Duration::from_secs(10 * 60));
        assert!(config.oidc.providers.is_empty());

        assert!(config.api_auth.enabled);
        assert_eq!(config.api_auth.api_key_header, "Authorization");
        assert!(!config.api_auth.allow_no_auth_fallback);
        assert_eq!(config.api_auth.no_auth_sentinel, NO_AUTH_SENTINEL);
        assert_eq!(config.api_auth.admin_token, None);
        assert_eq!(config.api_auth.jwt_secret, None);
        assert_eq!(
            config.api_auth.session_ttl,
            Duration::from_secs(24 * 60 * 60)
        );
        assert_eq!(config.api_auth.bcrypt_cost, 12);
    }

    #[test]
    fn duration_strings_parse_and_serialize() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            duration_format::parse_duration("30s")?,
            Duration::from_secs(30)
        );
        assert_eq!(
            duration_format::parse_duration("1m")?,
            Duration::from_secs(60)
        );
        assert_eq!(
            duration_format::parse_duration("1000ms")?,
            Duration::from_secs(1)
        );
        assert_eq!(
            duration_format::format_duration(Duration::from_secs(600)),
            "10m"
        );
        Ok(())
    }

    #[test]
    fn log_config_parses_from_yaml() -> Result<(), Box<dyn std::error::Error>> {
        let yaml = r#"
log:
  name: gateway
  level: debug
  format: pretty
"#;
        let config: AppConfig = serde_yaml::from_str(yaml)?;
        assert_eq!(config.log.name, "gateway");
        assert_eq!(config.log.level, "debug");
        assert_eq!(config.log.format, "pretty");
        assert_eq!(config.server.port, 8090);
        Ok(())
    }

    #[test]
    fn config_example_matches_model() -> Result<(), Box<dyn std::error::Error>> {
        let yaml = include_str!("../../../config.example.yml");
        let config: AppConfig = serde_yaml::from_str(yaml)?;
        assert_eq!(config.server.port, 8090);
        assert_eq!(config.api_auth.no_auth_sentinel, NO_AUTH_SENTINEL);
        Ok(())
    }

    /// P1-003 S12: the new Go-parity `ServerConfig` fields default to the exact
    /// values Go `conf.go` `setDefaults` produces. Source of truth:
    /// `conduit/conf/conf.go` lines 154-188 and
    /// `conduit/internal/server/config.go`.
    #[test]
    fn server_config_go_parity_fields_default_correctly() {
        let server = ServerConfig::default();

        // Direct Go fields.
        assert_eq!(
            server.public_url, "",
            "server.public_url Go default is \"\""
        );
        assert_eq!(
            server.request_timeout,
            Duration::from_secs(30),
            "server.request_timeout Go default is 30s"
        );
        assert_eq!(
            server.llm_request_timeout,
            Duration::from_secs(600),
            "server.llm_request_timeout Go default is 600s"
        );
        assert!(
            !server.disable_ssl_verify,
            "server.disable_ssl_verify Go default is false"
        );

        // trace sub-struct — Go tracing.Config defaults.
        assert_eq!(server.trace.thread_header, "Conduit-Thread-Id");
        assert_eq!(server.trace.trace_header, "Conduit-Trace-Id");
        assert_eq!(
            server.trace.request_header, "",
            "Go has no setDefault for request_header"
        );
        assert!(server.trace.extra_trace_headers.is_empty());
        assert!(server.trace.extra_trace_body_fields.is_empty());
        assert!(!server.trace.claude_code_trace_enabled);
        assert!(!server.trace.codex_trace_enabled);
        assert!(!server.trace.opencode_trace_enabled);

        // dashboard sub-struct — Go server.Dashboard defaults.
        assert_eq!(
            server.dashboard.all_time_token_stats_soft_ttl,
            Duration::from_secs(60 * 60),
            "dashboard.all_time_token_stats_soft_ttl Go default is 1h"
        );
        assert_eq!(
            server.dashboard.all_time_token_stats_hard_ttl,
            Duration::from_secs(24 * 60 * 60),
            "dashboard.all_time_token_stats_hard_ttl Go default is 24h"
        );

        // api.auth sub-struct — Go server.APIAuth defaults.
        assert!(
            !server.api.auth.allow_no_auth,
            "api.auth.allow_no_auth Go default is false"
        );
        assert_eq!(
            server.api.auth.key_prefix, "conduit",
            "api.auth.key_prefix uses the Conduit product prefix"
        );
    }

    /// P1-003 S12: the new Go-parity `ServerConfig` fields parse from a
    /// Go-shaped YAML payload (snake_case tags, duration strings).
    #[test]
    fn server_config_go_shaped_yaml_parses_new_fields() -> Result<(), serde_yaml::Error> {
        let yaml = r#"
public_url: "https://gateway.example.test"
request_timeout: 45s
llm_request_timeout: 900s
disable_ssl_verify: true
trace:
  thread_header: X-Thread
  trace_header: X-Trace
  request_header: X-Request
  extra_trace_headers: [Sentry-Trace]
  extra_trace_body_fields: [meta.trace_id]
  claude_code_trace_enabled: true
  codex_trace_enabled: true
  opencode_trace_enabled: true
dashboard:
  all_time_token_stats_soft_ttl: 2h
  all_time_token_stats_hard_ttl: 48h
api:
  auth:
    allow_no_auth: true
    key_prefix: sk
"#;
        let server: ServerConfig = serde_yaml::from_str(yaml)?;
        assert_eq!(server.public_url, "https://gateway.example.test");
        assert_eq!(server.request_timeout, Duration::from_secs(45));
        assert_eq!(server.llm_request_timeout, Duration::from_secs(900));
        assert!(server.disable_ssl_verify);
        assert_eq!(server.trace.thread_header, "X-Thread");
        assert_eq!(server.trace.trace_header, "X-Trace");
        assert_eq!(server.trace.request_header, "X-Request");
        assert_eq!(
            server.trace.extra_trace_headers,
            vec!["Sentry-Trace".to_string()]
        );
        assert_eq!(
            server.trace.extra_trace_body_fields,
            vec!["meta.trace_id".to_string()]
        );
        assert!(server.trace.claude_code_trace_enabled);
        assert!(server.trace.codex_trace_enabled);
        assert!(server.trace.opencode_trace_enabled);
        assert_eq!(
            server.dashboard.all_time_token_stats_soft_ttl,
            Duration::from_secs(2 * 60 * 60)
        );
        assert_eq!(
            server.dashboard.all_time_token_stats_hard_ttl,
            Duration::from_secs(48 * 60 * 60)
        );
        assert!(server.api.auth.allow_no_auth);
        assert_eq!(server.api.auth.key_prefix, "sk");
        // Fields absent from yaml fall back to defaults.
        assert_eq!(server.host, "127.0.0.1");
        assert_eq!(server.port, 8090);
        Ok(())
    }

    /// P1-003 S12: `ServerConfig` round-trips through serde (the new duration
    /// and nested-struct fields must serialize + deserialize losslessly).
    #[test]
    fn server_config_round_trips_new_fields_through_serde_json() -> Result<(), serde_json::Error> {
        let original = ServerConfig::default();
        let json = serde_json::to_string(&original)?;
        let parsed: ServerConfig = serde_json::from_str(&json)?;
        assert_eq!(parsed, original);
        // Spot-check that the new duration fields serialize as Go-style strings.
        // `format_duration` emits the largest whole unit (600s -> "10m",
        // 30s -> "30s", 3600s -> "1h", 86400s -> "1d"), matching Go's
        // `time.Duration.String`-adjacent style used by the YAML example.
        assert!(json.contains("\"request_timeout\":\"30s\""));
        assert!(json.contains("\"llm_request_timeout\":\"10m\""));
        assert!(json.contains("\"all_time_token_stats_soft_ttl\":\"1h\""));
        assert!(json.contains("\"all_time_token_stats_hard_ttl\":\"1d\""));
        Ok(())
    }

    /// P1-003 S12: the new Go-parity `LogConfig` fields default to the exact
    /// values Go `conf.go` `setDefaults` produces. Source of truth:
    /// `conduit/conf/conf.go` lines 205-222 and
    /// `conduit/internal/log/logger.go` `FileConfig`.
    #[test]
    fn log_config_go_parity_fields_default_correctly() {
        let log = LogConfig::default();

        // Direct Go log.Config fields.
        assert_eq!(log.name, "conduit", "log.name Go default is conduit");
        assert!(!log.debug, "log.debug Go default is false");
        assert_eq!(log.skip_level, 1, "log.skip_level Go default is 1");
        assert_eq!(log.level, "info", "log.level Go default is info");
        assert_eq!(log.level_key, "level");
        assert_eq!(log.time_key, "time");
        assert_eq!(log.caller_key, "label");
        assert_eq!(log.function_key, "");
        assert_eq!(log.name_key, "logger");
        assert_eq!(log.encoding, "json", "log.encoding Go default is json");
        assert!(log.includes.is_empty());
        assert!(log.excludes.is_empty());
        assert_eq!(log.output, "stdio", "log.output Go default is stdio");

        // file sub-struct — Go log.FileConfig defaults.
        assert_eq!(log.file.path, "logs/conduit.log");
        assert_eq!(log.file.max_size, 100);
        assert_eq!(log.file.max_age, 30);
        assert_eq!(log.file.max_backups, 10);
        assert!(log.file.local_time);

        // Rust-only extras still default as before.
        assert_eq!(log.format, "json");
        assert_eq!(log.directory, "logs");
        assert!(log.stdout);
    }

    /// P1-003 S12: the new Go-parity `LogConfig` fields parse from a Go-shaped
    /// YAML payload (mirrors the Go `config.example.yml` `log:` section).
    #[test]
    fn log_config_go_shaped_yaml_parses_new_fields() -> Result<(), serde_yaml::Error> {
        let yaml = r#"
name: gateway
debug: true
skip_level: 2
level: debug
level_key: severity
time_key: ts
caller_key: caller
function_key: fn
name_key: logger_name
encoding: console
includes: [conduit/server]
excludes: [conduit/noisy]
output: file
file:
  path: /var/log/conduit/conduit.log
  max_size: 200
  max_age: 14
  max_backups: 7
  local_time: false
"#;
        let log: LogConfig = serde_yaml::from_str(yaml)?;
        assert_eq!(log.name, "gateway");
        assert!(log.debug);
        assert_eq!(log.skip_level, 2);
        assert_eq!(log.level, "debug");
        assert_eq!(log.level_key, "severity");
        assert_eq!(log.time_key, "ts");
        assert_eq!(log.caller_key, "caller");
        assert_eq!(log.function_key, "fn");
        assert_eq!(log.name_key, "logger_name");
        assert_eq!(log.encoding, "console");
        assert_eq!(log.includes, vec!["conduit/server".to_string()]);
        assert_eq!(log.excludes, vec!["conduit/noisy".to_string()]);
        assert_eq!(log.output, "file");
        assert_eq!(log.file.path, "/var/log/conduit/conduit.log");
        assert_eq!(log.file.max_size, 200);
        assert_eq!(log.file.max_age, 14);
        assert_eq!(log.file.max_backups, 7);
        assert!(!log.file.local_time);
        // Rust-only extras absent from yaml fall back to defaults.
        assert_eq!(log.format, "json");
        assert_eq!(log.directory, "logs");
        assert!(log.stdout);
        Ok(())
    }
}

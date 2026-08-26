#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

pub mod entry;
pub mod live_cache;
pub mod memory;
pub mod noop;
pub mod redis;
pub mod two_level;

pub use entry::CacheEntry;
pub use live_cache::{
    LiveCache, LiveCacheDimension, LiveCacheInvalidationEvent, LiveCacheInvalidationTarget,
    LivePreviewEntry, LocalLiveCacheInvalidationWatcher, invalidate_cache_target,
    invalidation_key_prefix, invalidation_topic,
};
pub use memory::MemoryCache;
pub use noop::NoopCache;
pub use redis::{RedisCache, RedisConnectionConfig, RedisMode};
pub use two_level::TwoLevelCache;

pub type CacheResult<T> = Result<T, CacheError>;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("cache backend is unavailable: {0}")]
    Unavailable(String),
    #[error("cache operation is not supported by this backend: {0}")]
    Unsupported(String),
    #[error("invalid cache config: {0}")]
    InvalidConfig(String),
    #[error("cache serialization failed: {0}")]
    Serialization(String),
    /// Go parity: `xcache.ErrCacheNotConfigured` (noop.go:11). Returned by
    /// the Go `noopCache.Get` to signal "cache not configured"; Rust callers
    /// that treat a noop cache as a perpetual miss can match on this variant
    /// (or simply rely on [`NoopCache::get`] returning `Ok(None)`, which is
    /// the more idiomatic Rust mapping chosen by the existing service layer).
    #[error("cache not configured")]
    NotConfigured,
}

#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>>;

    async fn set(&self, key: &str, value: Value, ttl: Option<Duration>) -> CacheResult<()>;

    async fn delete(&self, key: &str) -> CacheResult<()>;

    async fn invalidate_prefix(&self, prefix: &str) -> CacheResult<()>;

    /// Go parity: `Cache.GetType()` — returns a short diagnostic identifier
    /// for the backend (`"cache"` for memory/redis, `"chain"` for two-level,
    /// `"noop"` for the no-op cache). Used by Go tests for backend
    /// discrimination (e.g. cache_test.go:33, noop_test.go:41). The default
    /// is `"cache"` to match the gocache default that the Go memory/redis
    /// setters expose.
    fn get_type(&self) -> &'static str {
        "cache"
    }

    /// Go parity: `Cache.Clear(ctx)` — removes every entry from the cache.
    /// The default is a no-op so backends that cannot enumerate keys (the
    /// not-yet-wired Redis backend, the prefixed wrapper when the inner
    /// backend declines) still satisfy the trait without panicking.
    async fn clear(&self) -> CacheResult<()> {
        Ok(())
    }
}

#[async_trait]
pub trait CacheJsonExt: Cache {
    async fn get_json<T>(&self, key: &str) -> CacheResult<Option<T>>
    where
        T: DeserializeOwned + Send,
    {
        let Some(value) = self.get(key).await? else {
            return Ok(None);
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(cache_json_error)
    }

    async fn set_json<T>(&self, key: &str, value: &T, ttl: Option<Duration>) -> CacheResult<()>
    where
        T: Serialize + Sync,
    {
        let value = serde_json::to_value(value).map_err(cache_json_error)?;
        self.set(key, value, ttl).await
    }
}

#[async_trait]
impl<T> CacheJsonExt for T where T: Cache + ?Sized {}

fn cache_json_error(error: serde_json::Error) -> CacheError {
    CacheError::Serialization(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Noop,
    Memory,
    Redis,
    TwoLevel,
}

impl CacheMode {
    pub fn from_name(input: &str) -> CacheResult<Self> {
        Self::from_str(input)
    }
}

impl FromStr for CacheMode {
    type Err = CacheError;

    fn from_str(input: &str) -> CacheResult<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "" | "empty" | "invalid" | "noop" | "none" | "disabled" | "off" => Ok(Self::Noop),
            "memory" | "mem" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            "two-level" | "two_level" | "twolevel" | "tiered" => Ok(Self::TwoLevel),
            other => Err(CacheError::InvalidConfig(format!(
                "unsupported cache mode {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryCacheConfig {
    pub default_ttl: Duration,
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheConfig {
    pub mode: CacheMode,
    pub memory: MemoryCacheConfig,
    pub redis: RedisConnectionConfig,
    pub key_prefix: Option<String>,
}

impl CacheConfig {
    pub fn new(mode: CacheMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    pub fn from_mode_name(mode: &str) -> CacheResult<Self> {
        Ok(Self::new(CacheMode::from_name(mode)?))
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            mode: CacheMode::Noop,
            memory: MemoryCacheConfig::default(),
            redis: RedisConnectionConfig::default(),
            key_prefix: None,
        }
    }
}

pub fn build_cache(config: CacheConfig) -> CacheResult<Arc<dyn Cache>> {
    let cache: Arc<dyn Cache> = match config.mode {
        CacheMode::Noop => Arc::new(NoopCache::new()),
        CacheMode::Memory => Arc::new(MemoryCache::new(config.memory.default_ttl)),
        CacheMode::Redis => Arc::new(RedisCache::new(config.redis)?),
        CacheMode::TwoLevel => {
            let local = Arc::new(MemoryCache::new(config.memory.default_ttl));
            let remote = Arc::new(RedisCache::new(config.redis)?);
            Arc::new(TwoLevelCache::new(local, remote))
        }
    };

    if let Some(prefix) = config.key_prefix.filter(|prefix| !prefix.is_empty()) {
        Ok(Arc::new(PrefixedCache::new(
            cache,
            CacheNamespace::new(prefix),
        )))
    } else {
        Ok(cache)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheNamespace {
    prefix: String,
}

impl CacheNamespace {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into().trim_matches(':').to_string(),
        }
    }

    pub fn key(&self, key: &str) -> String {
        namespaced_key(&self.prefix, key)
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }
}

pub fn namespaced_key(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches(':');
    let key = key.trim_start_matches(':');
    if prefix.is_empty() {
        key.to_string()
    } else if key.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}:{key}")
    }
}

#[derive(Clone)]
pub struct PrefixedCache {
    inner: Arc<dyn Cache>,
    namespace: CacheNamespace,
}

impl PrefixedCache {
    pub fn new(inner: Arc<dyn Cache>, namespace: CacheNamespace) -> Self {
        Self { inner, namespace }
    }

    pub const fn namespace(&self) -> &CacheNamespace {
        &self.namespace
    }
}

#[async_trait]
impl Cache for PrefixedCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        self.inner.get(&self.namespace.key(key)).await
    }

    async fn set(&self, key: &str, value: Value, ttl: Option<Duration>) -> CacheResult<()> {
        self.inner.set(&self.namespace.key(key), value, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<()> {
        self.inner.delete(&self.namespace.key(key)).await
    }

    async fn invalidate_prefix(&self, prefix: &str) -> CacheResult<()> {
        // Prefix invalidation must stay inside this namespace, even for short caller prefixes.
        self.inner
            .invalidate_prefix(&self.namespace.key(prefix))
            .await
    }

    fn get_type(&self) -> &'static str {
        self.inner.get_type()
    }

    async fn clear(&self) -> CacheResult<()> {
        self.inner.clear().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct TypedValue {
        id: u64,
        name: String,
    }

    #[test]
    fn cache_mode_parses_supported_modes() -> CacheResult<()> {
        assert_eq!(CacheMode::from_name("")?, CacheMode::Noop);
        assert_eq!(CacheMode::from_name("invalid")?, CacheMode::Noop);
        assert_eq!(CacheMode::from_name("memory")?, CacheMode::Memory);
        assert_eq!(CacheMode::from_name("redis")?, CacheMode::Redis);
        assert_eq!(CacheMode::from_name("two_level")?, CacheMode::TwoLevel);
        Ok(())
    }

    #[test]
    fn cache_mode_rejects_unknown_modes() {
        let err = CacheMode::from_name("mystery")
            .err()
            .map(|err| err.to_string());
        assert_eq!(
            err,
            Some("invalid cache config: unsupported cache mode \"mystery\"".to_string())
        );
    }

    #[test]
    fn namespace_normalizes_key_prefix() {
        let namespace = CacheNamespace::new(":live:");
        assert_eq!(namespace.prefix(), "live");
        assert_eq!(namespace.key(":request:1"), "live:request:1");
        assert_eq!(namespaced_key("", "plain"), "plain");
    }

    #[tokio::test]
    async fn json_helpers_round_trip_typed_values() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_secs(60));
        let value = TypedValue {
            id: 7,
            name: "alpha".to_string(),
        };

        cache.set_json("typed", &value, None).await?;
        assert_eq!(cache.get_json("typed").await?, Some(value));

        Ok(())
    }

    #[tokio::test]
    async fn factory_builds_prefixed_memory_cache() -> CacheResult<()> {
        let raw = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let cache = PrefixedCache::new(raw.clone(), CacheNamespace::new("tenant-a"));

        cache.set("item", json!(1), None).await?;

        assert_eq!(cache.get("item").await?, Some(json!(1)));
        assert_eq!(raw.get("tenant-a:item").await?, Some(json!(1)));

        Ok(())
    }

    #[test]
    fn factory_handles_cluster_mode_according_to_compiled_feature() {
        let redis = RedisConnectionConfig {
            mode: RedisMode::Cluster,
            ..RedisConnectionConfig::default()
        };
        let config = CacheConfig {
            mode: CacheMode::Redis,
            redis,
            ..CacheConfig::default()
        };

        let result = build_cache(config);
        #[cfg(feature = "redis")]
        assert!(result.is_ok());
        #[cfg(not(feature = "redis"))]
        assert!(matches!(result, Err(CacheError::Unsupported(_))));
    }

    #[tokio::test]
    async fn noop_and_memory_report_distinct_get_type_values() -> CacheResult<()> {
        let noop: Arc<dyn Cache> = Arc::new(NoopCache::new());
        let memory: Arc<dyn Cache> = Arc::new(MemoryCache::new(Duration::from_secs(60)));

        assert_eq!(noop.get_type(), "noop");
        assert_eq!(memory.get_type(), "cache");

        #[cfg(feature = "redis")]
        {
            let redis = RedisCache::from_url("redis://127.0.0.1:6379")?;
            assert_eq!(<RedisCache as Cache>::get_type(&redis), "cache");
        }
        Ok(())
    }

    #[tokio::test]
    async fn prefixed_cache_delegates_get_type_and_clear() -> CacheResult<()> {
        let inner: Arc<dyn Cache> = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let prefixed = PrefixedCache::new(inner.clone(), CacheNamespace::new("ns"));

        prefixed.set("k", json!(1), None).await?;
        assert_eq!(prefixed.get_type(), inner.get_type());
        prefixed.clear().await?;
        assert_eq!(prefixed.get("k").await?, None);
        Ok(())
    }
}

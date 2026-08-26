use crate::{Cache, CacheResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct NoopCache;

impl NoopCache {
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Cache for NoopCache {
    async fn get(&self, _key: &str) -> CacheResult<Option<Value>> {
        // Parity note: Go `noopCache.Get` returns
        // `store.NotFoundWithCause(ErrCacheNotConfigured)` (noop.go:25-28),
        // which Go services match with `if err == nil`. The Rust service
        // layer instead treats a noop cache as a perpetual miss, so we
        // return `Ok(None)` (cache-miss) here. Callers that need to
        // distinguish "noop backend" from "key absent in a real backend"
        // can inspect [`Self::get_type`] (`"noop"`) or match on
        // [`crate::CacheError::NotConfigured`].
        Ok(None)
    }

    async fn set(&self, _key: &str, _value: Value, _ttl: Option<Duration>) -> CacheResult<()> {
        Ok(())
    }

    async fn delete(&self, _key: &str) -> CacheResult<()> {
        Ok(())
    }

    async fn invalidate_prefix(&self, _prefix: &str) -> CacheResult<()> {
        Ok(())
    }

    /// Go parity: `noopCache.GetType()` returns `"noop"` (noop.go:51-53).
    fn get_type(&self) -> &'static str {
        "noop"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn noop_never_returns_values() -> CacheResult<()> {
        let cache = NoopCache::new();

        cache.set("key", json!({"stored": true}), None).await?;
        assert_eq!(cache.get("key").await?, None);
        cache.delete("key").await?;
        cache.invalidate_prefix("key").await?;
        cache.clear().await?;

        assert_eq!(cache.get_type(), "noop");
        Ok(())
    }
}

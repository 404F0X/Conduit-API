use crate::{Cache, CacheResult};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct TwoLevelCache {
    local: Arc<dyn Cache>,
    remote: Arc<dyn Cache>,
}

impl TwoLevelCache {
    pub fn new(local: Arc<dyn Cache>, remote: Arc<dyn Cache>) -> Self {
        Self { local, remote }
    }
}

#[async_trait]
impl Cache for TwoLevelCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        if let Some(value) = self.local.get(key).await? {
            return Ok(Some(value));
        }

        let value = self.remote.get(key).await?;
        if let Some(value) = value.as_ref() {
            // Backfill local memory so repeated reads do not hit the remote cache.
            self.local.set(key, value.clone(), None).await?;
        }
        Ok(value)
    }

    async fn set(&self, key: &str, value: Value, ttl: Option<Duration>) -> CacheResult<()> {
        self.remote.set(key, value.clone(), ttl).await?;
        self.local.set(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> CacheResult<()> {
        self.remote.delete(key).await?;
        self.local.delete(key).await
    }

    async fn invalidate_prefix(&self, prefix: &str) -> CacheResult<()> {
        self.remote.invalidate_prefix(prefix).await?;
        self.local.invalidate_prefix(prefix).await
    }

    /// Go parity: `chain` cache `GetType()` returns `"chain"`
    /// (cache_test.go:130). Two-level = chain of memory+redis in the Go
    /// codebase (cache.go:147-148).
    fn get_type(&self) -> &'static str {
        "chain"
    }

    /// Go parity: `Cache.Clear(ctx)` on a chain clears every level. The
    /// gocache chain fans out Clear to every constituent cache.
    async fn clear(&self) -> CacheResult<()> {
        self.remote.clear().await?;
        self.local.clear().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryCache;
    use serde_json::json;

    #[tokio::test]
    async fn two_level_writes_to_local_and_remote() -> CacheResult<()> {
        let local = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let remote = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let cache = TwoLevelCache::new(local.clone(), remote.clone());

        cache.set("key", json!({"stored": true}), None).await?;

        assert_eq!(local.get("key").await?, Some(json!({"stored": true})));
        assert_eq!(remote.get("key").await?, Some(json!({"stored": true})));

        Ok(())
    }

    #[tokio::test]
    async fn two_level_reads_remote_after_local_miss_and_backfills() -> CacheResult<()> {
        let local = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let remote = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let cache = TwoLevelCache::new(local.clone(), remote.clone());

        remote.set("key", json!("remote"), None).await?;

        assert_eq!(cache.get("key").await?, Some(json!("remote")));
        assert_eq!(local.get("key").await?, Some(json!("remote")));

        Ok(())
    }

    #[tokio::test]
    async fn two_level_clears_both_levels_and_reports_chain_type() -> CacheResult<()> {
        let local = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let remote = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let cache = TwoLevelCache::new(local.clone(), remote.clone());

        cache.set("key", json!("v"), None).await?;
        cache.clear().await?;

        assert_eq!(local.get("key").await?, None);
        assert_eq!(remote.get("key").await?, None);
        assert_eq!(cache.get_type(), "chain");
        Ok(())
    }
}

use crate::{Cache, CacheResult};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct MemoryCache {
    entries: Mutex<HashMap<String, MemoryEntry>>,
    default_ttl: Duration,
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    value: Value,
    expires_at: Option<Instant>,
}

impl MemoryCache {
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            default_ttl,
        }
    }

    pub fn with_default_ttl() -> Self {
        Self::new(Duration::from_secs(5))
    }

    fn expires_at(&self, ttl: Option<Duration>) -> Option<Instant> {
        let ttl = ttl.unwrap_or(self.default_ttl);
        if ttl.is_zero() {
            None
        } else {
            Some(Instant::now() + ttl)
        }
    }
}

impl Default for MemoryCache {
    fn default() -> Self {
        Self::with_default_ttl()
    }
}

#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get(key) else {
            return Ok(None);
        };

        if entry
            .expires_at
            .is_some_and(|expires_at| expires_at <= Instant::now())
        {
            entries.remove(key);
            return Ok(None);
        }

        Ok(Some(entry.value.clone()))
    }

    async fn set(&self, key: &str, value: Value, ttl: Option<Duration>) -> CacheResult<()> {
        let entry = MemoryEntry {
            value,
            expires_at: self.expires_at(ttl),
        };
        self.entries.lock().await.insert(key.to_string(), entry);
        Ok(())
    }

    async fn delete(&self, key: &str) -> CacheResult<()> {
        self.entries.lock().await.remove(key);
        Ok(())
    }

    async fn invalidate_prefix(&self, prefix: &str) -> CacheResult<()> {
        self.entries
            .lock()
            .await
            .retain(|key, _entry| !key.starts_with(prefix));
        Ok(())
    }

    /// Go parity: gocache `cache` backend `GetType()` returns `"cache"`
    /// (cache_test.go:33). Explicit override for clarity.
    fn get_type(&self) -> &'static str {
        "cache"
    }

    /// Go parity: `Cache.Clear(ctx)` — drops every entry. The gocache
    /// memory backend implements this by deleting all items.
    async fn clear(&self) -> CacheResult<()> {
        self.entries.lock().await.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn memory_cache_get_set_delete() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_secs(60));

        cache.set("api:key:1", json!({"id": 1}), None).await?;
        assert_eq!(cache.get("api:key:1").await?, Some(json!({"id": 1})));

        cache.delete("api:key:1").await?;
        assert_eq!(cache.get("api:key:1").await?, None);

        Ok(())
    }

    #[tokio::test]
    async fn memory_cache_invalidate_prefix() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_secs(60));

        cache.set("channel:1", json!(1), None).await?;
        cache.set("channel:2", json!(2), None).await?;
        cache.set("system:1", json!(3), None).await?;
        cache.invalidate_prefix("channel:").await?;

        assert_eq!(cache.get("channel:1").await?, None);
        assert_eq!(cache.get("channel:2").await?, None);
        assert_eq!(cache.get("system:1").await?, Some(json!(3)));

        Ok(())
    }

    #[tokio::test]
    async fn memory_cache_expires_entries() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_millis(5));

        cache.set("short", json!(true), None).await?;
        tokio::time::sleep(Duration::from_millis(15)).await;

        assert_eq!(cache.get("short").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn memory_cache_default_ttl_is_five_seconds() -> CacheResult<()> {
        let cache = MemoryCache::default();

        cache.set("default-ttl", json!(true), None).await?;

        assert_eq!(cache.get("default-ttl").await?, Some(json!(true)));
        assert_eq!(cache.default_ttl, Duration::from_secs(5));
        Ok(())
    }

    #[tokio::test]
    async fn memory_cache_clear_drops_all_entries() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_secs(60));

        cache.set("a", json!(1), None).await?;
        cache.set("b", json!(2), None).await?;
        cache.set("c", json!(3), None).await?;

        cache.clear().await?;

        assert_eq!(cache.get("a").await?, None);
        assert_eq!(cache.get("b").await?, None);
        assert_eq!(cache.get("c").await?, None);
        assert_eq!(cache.get_type(), "cache");
        Ok(())
    }

    #[tokio::test]
    async fn memory_cache_concurrent_set_get_stays_consistent() -> CacheResult<()> {
        // Mirror the spirit of Go's race tests (api_key_race_test.go) without
        // requiring `-race`: many parallel writers/readers must not deadlock
        // or panic. The mutex-backed MemoryCache serializes access cleanly.
        let cache = std::sync::Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let mut handles = Vec::new();

        for index in 0..16u32 {
            let cache = std::sync::Arc::clone(&cache);
            handles.push(tokio::spawn(async move {
                let key = format!("k:{index}");
                cache.set(&key, json!(index), None).await?;
                let value = cache.get(&key).await?;
                assert_eq!(value, Some(json!(index)));
                cache.delete(&key).await?;
                assert_eq!(cache.get(&key).await?, None);
                Ok::<_, crate::CacheError>(())
            }));
        }

        for handle in handles {
            handle.await.map_err(|err| {
                crate::CacheError::Unavailable(format!("task join failed: {err}"))
            })??;
        }

        // After every writer deleted its own key, the cache should be empty.
        cache.clear().await?;
        Ok(())
    }
}

use crate::{Cache, CacheResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

const DEFAULT_INVALIDATION_BUFFER: usize = 128;

#[derive(Debug, Clone, PartialEq)]
pub struct LivePreviewEntry {
    pub request_id: String,
    pub sequence: u64,
    pub payload: Value,
    pub created_at_unix_ms: u128,
}

impl LivePreviewEntry {
    pub fn new(request_id: impl Into<String>, sequence: u64, payload: Value) -> Self {
        Self {
            request_id: request_id.into(),
            sequence,
            payload,
            created_at_unix_ms: now_unix_ms(),
        }
    }

    pub fn cache_key(&self) -> String {
        live_preview_key(&self.request_id, self.sequence)
    }

    pub fn into_value(self) -> Value {
        json!({
            "request_id": self.request_id,
            "sequence": self.sequence,
            "payload": self.payload,
            "created_at_unix_ms": self.created_at_unix_ms,
        })
    }
}

#[derive(Clone)]
pub struct LiveCache {
    cache: Arc<dyn Cache>,
    ttl: Duration,
    invalidations: Arc<LocalInvalidationBus>,
}

impl LiveCache {
    pub fn new(cache: Arc<dyn Cache>, ttl: Duration) -> Self {
        Self::with_invalidation_buffer(cache, ttl, DEFAULT_INVALIDATION_BUFFER)
    }

    pub fn with_invalidation_buffer(
        cache: Arc<dyn Cache>,
        ttl: Duration,
        invalidation_buffer: usize,
    ) -> Self {
        Self {
            cache,
            ttl,
            invalidations: Arc::new(LocalInvalidationBus::new(invalidation_buffer)),
        }
    }

    pub async fn push(&self, entry: LivePreviewEntry) -> CacheResult<()> {
        self.cache
            .set(&entry.cache_key(), entry.into_value(), Some(self.ttl))
            .await
    }

    pub async fn get_chunk(&self, request_id: &str, sequence: u64) -> CacheResult<Option<Value>> {
        self.cache
            .get(&live_preview_key(request_id, sequence))
            .await
    }

    pub async fn invalidate_request(&self, request_id: &str) -> CacheResult<()> {
        self.cache
            .invalidate_prefix(&format!("live_preview:{request_id}:"))
            .await
    }

    pub fn subscribe_invalidations(&self) -> LocalLiveCacheInvalidationWatcher {
        LocalLiveCacheInvalidationWatcher {
            receiver: self.invalidations.sender.subscribe(),
        }
    }

    pub async fn invalidate_dimension(
        &self,
        dimension: LiveCacheDimension,
        id_or_slug: impl Into<String>,
        reason: impl Into<String>,
    ) -> CacheResult<LiveCacheInvalidationEvent> {
        self.invalidate_target(
            LiveCacheInvalidationTarget::new(dimension, id_or_slug),
            reason,
        )
        .await
    }

    pub async fn invalidate_target(
        &self,
        target: LiveCacheInvalidationTarget,
        reason: impl Into<String>,
    ) -> CacheResult<LiveCacheInvalidationEvent> {
        invalidate_cache_target(self.cache.as_ref(), &target).await?;

        let event = LiveCacheInvalidationEvent::new(
            target.dimension,
            target.id_or_slug,
            reason,
            self.invalidations.next_sequence(),
        );
        self.publish_invalidation(event.clone());
        Ok(event)
    }

    pub fn publish_invalidation(&self, event: LiveCacheInvalidationEvent) {
        // A local fake bus should not fail invalidation just because no watcher is active.
        let _ = self.invalidations.sender.send(event);
    }
}

#[derive(Debug)]
struct LocalInvalidationBus {
    sender: broadcast::Sender<LiveCacheInvalidationEvent>,
    next_sequence: AtomicU64,
}

impl LocalInvalidationBus {
    fn new(buffer: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer.max(1));
        Self {
            sender,
            next_sequence: AtomicU64::new(1),
        }
    }

    fn next_sequence(&self) -> u64 {
        self.next_sequence.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct LocalLiveCacheInvalidationWatcher {
    receiver: broadcast::Receiver<LiveCacheInvalidationEvent>,
}

impl LocalLiveCacheInvalidationWatcher {
    pub async fn recv(
        &mut self,
    ) -> Result<LiveCacheInvalidationEvent, broadcast::error::RecvError> {
        self.receiver.recv().await
    }

    pub fn try_recv(
        &mut self,
    ) -> Result<LiveCacheInvalidationEvent, broadcast::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LiveCacheDimension {
    Channel,
    Model,
    ApiKey,
    System,
    Custom(String),
}

impl LiveCacheDimension {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Channel => "channel",
            Self::Model => "model",
            Self::ApiKey => "api_key",
            Self::System => "system",
            Self::Custom(dimension) => dimension.as_str(),
        }
    }
}

impl fmt::Display for LiveCacheDimension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveCacheInvalidationTarget {
    pub dimension: LiveCacheDimension,
    pub id_or_slug: String,
}

impl LiveCacheInvalidationTarget {
    pub fn new(dimension: LiveCacheDimension, id_or_slug: impl Into<String>) -> Self {
        Self {
            dimension,
            id_or_slug: normalize_cache_segment(id_or_slug),
        }
    }

    pub fn channel(id_or_slug: impl Into<String>) -> Self {
        Self::new(LiveCacheDimension::Channel, id_or_slug)
    }

    pub fn model(id_or_slug: impl Into<String>) -> Self {
        Self::new(LiveCacheDimension::Model, id_or_slug)
    }

    pub fn api_key(id_or_slug: impl Into<String>) -> Self {
        Self::new(LiveCacheDimension::ApiKey, id_or_slug)
    }

    pub fn system(id_or_slug: impl Into<String>) -> Self {
        Self::new(LiveCacheDimension::System, id_or_slug)
    }

    pub fn topic(&self) -> String {
        invalidation_topic(&self.dimension, &self.id_or_slug)
    }

    pub fn key_prefix(&self) -> String {
        invalidation_key_prefix(&self.dimension, &self.id_or_slug)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveCacheInvalidationEvent {
    pub dimension: LiveCacheDimension,
    pub id_or_slug: String,
    pub reason: String,
    pub sequence: u64,
    pub timestamp_unix_ms: u128,
}

impl LiveCacheInvalidationEvent {
    pub fn new(
        dimension: LiveCacheDimension,
        id_or_slug: impl Into<String>,
        reason: impl Into<String>,
        sequence: u64,
    ) -> Self {
        Self {
            dimension,
            id_or_slug: normalize_cache_segment(id_or_slug),
            reason: reason.into(),
            sequence,
            timestamp_unix_ms: now_unix_ms(),
        }
    }

    pub fn target(&self) -> LiveCacheInvalidationTarget {
        LiveCacheInvalidationTarget::new(self.dimension.clone(), self.id_or_slug.clone())
    }

    pub fn topic(&self) -> String {
        invalidation_topic(&self.dimension, &self.id_or_slug)
    }

    pub fn key_prefix(&self) -> String {
        invalidation_key_prefix(&self.dimension, &self.id_or_slug)
    }
}

pub fn invalidation_topic(dimension: &LiveCacheDimension, id_or_slug: &str) -> String {
    format!(
        "live_cache:invalidation:{}:{}",
        dimension,
        normalize_cache_segment(id_or_slug)
    )
}

pub fn invalidation_key_prefix(dimension: &LiveCacheDimension, id_or_slug: &str) -> String {
    format!("{}:{}:", dimension, normalize_cache_segment(id_or_slug))
}

pub async fn invalidate_cache_target(
    cache: &dyn Cache,
    target: &LiveCacheInvalidationTarget,
) -> CacheResult<()> {
    cache.invalidate_prefix(&target.key_prefix()).await
}

fn live_preview_key(request_id: &str, sequence: u64) -> String {
    format!("live_preview:{request_id}:{sequence}")
}

fn normalize_cache_segment(value: impl Into<String>) -> String {
    // Keep helper output composable with colon-delimited cache keys and topics.
    value.into().trim_matches(':').to_string()
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryCache;
    use serde_json::json;

    fn recv_error(error: impl fmt::Display) -> crate::CacheError {
        crate::CacheError::Unavailable(error.to_string())
    }

    #[test]
    fn invalidation_target_builds_topic_and_key_prefix() {
        let channel = LiveCacheInvalidationTarget::channel(":primary:");
        let model = LiveCacheInvalidationTarget::model("gpt-4.1");
        let api_key = LiveCacheInvalidationTarget::api_key("key-7");
        let system = LiveCacheInvalidationTarget::system("settings");

        assert_eq!(channel.topic(), "live_cache:invalidation:channel:primary");
        assert_eq!(channel.key_prefix(), "channel:primary:");
        assert_eq!(model.key_prefix(), "model:gpt-4.1:");
        assert_eq!(api_key.key_prefix(), "api_key:key-7:");
        assert_eq!(system.key_prefix(), "system:settings:");
    }

    #[tokio::test]
    async fn invalidating_target_uses_cache_prefix_and_publishes_event() -> CacheResult<()> {
        let cache = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let live_cache = LiveCache::new(cache.clone(), Duration::from_secs(5));
        let mut watcher = live_cache.subscribe_invalidations();

        cache
            .set("channel:primary:config", json!({"enabled": true}), None)
            .await?;
        cache
            .set("model:gpt-4.1:config", json!({"enabled": true}), None)
            .await?;

        let event = live_cache
            .invalidate_target(
                LiveCacheInvalidationTarget::channel("primary"),
                "channel updated",
            )
            .await?;

        assert_eq!(cache.get("channel:primary:config").await?, None);
        assert_eq!(
            cache.get("model:gpt-4.1:config").await?,
            Some(json!({"enabled": true}))
        );
        assert_eq!(event.dimension, LiveCacheDimension::Channel);
        assert_eq!(event.id_or_slug, "primary");
        assert_eq!(event.reason, "channel updated");
        assert_eq!(event.sequence, 1);

        let received = tokio::time::timeout(Duration::from_secs(1), watcher.recv())
            .await
            .map_err(recv_error)?
            .map_err(recv_error)?;
        assert_eq!(received, event);

        Ok(())
    }

    #[tokio::test]
    async fn cache_target_helper_composes_with_cache_trait() -> CacheResult<()> {
        let cache = MemoryCache::new(Duration::from_secs(60));
        let target = LiveCacheInvalidationTarget::api_key("key-7");

        cache.set("api_key:key-7:quota", json!(10), None).await?;
        cache.set("api_key:key-8:quota", json!(20), None).await?;

        invalidate_cache_target(&cache, &target).await?;

        assert_eq!(cache.get("api_key:key-7:quota").await?, None);
        assert_eq!(cache.get("api_key:key-8:quota").await?, Some(json!(20)));

        Ok(())
    }
}

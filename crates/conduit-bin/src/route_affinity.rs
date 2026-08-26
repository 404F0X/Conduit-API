//! Shared cache/DB runtime for explicit route affinity.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use conduit_cache::Cache;
use conduit_db::{
    PolicyContext, Principal, RepoResult, RequestContext, RouteAffinityKey, RouteAffinityRepo,
    RouteAffinityRow, UpsertRouteAffinityInput,
};
use sha2::{Digest, Sha256};
use tracing::warn;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct RouteAffinityCacheEntry {
    row: Option<RouteAffinityRow>,
    valid_until: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct StickyChannelCacheEntry {
    channel_id: Option<String>,
    valid_until: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StickyChannelCacheState {
    Fresh(Option<String>),
    Expired,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RouteAffinityRuntimeConfig {
    pub prompt_cache_ttl: Duration,
    pub response_continuity_ttl: Duration,
    pub lookup_cache_ttl: Duration,
    pub negative_cache_ttl: Duration,
}

impl From<&conduit_config::model::RouteAffinityConfig> for RouteAffinityRuntimeConfig {
    fn from(value: &conduit_config::model::RouteAffinityConfig) -> Self {
        Self {
            prompt_cache_ttl: value.prompt_cache_ttl,
            response_continuity_ttl: value.response_continuity_ttl,
            lookup_cache_ttl: value.lookup_cache_ttl,
            negative_cache_ttl: value.negative_cache_ttl,
        }
    }
}

pub(crate) struct RouteAffinityRuntime {
    repo: Arc<dyn RouteAffinityRepo>,
    cache: Arc<dyn Cache>,
    config: RouteAffinityRuntimeConfig,
}

impl RouteAffinityRuntime {
    pub(crate) fn new(
        repo: Arc<dyn RouteAffinityRepo>,
        cache: Arc<dyn Cache>,
        config: RouteAffinityRuntimeConfig,
    ) -> Self {
        Self {
            repo,
            cache,
            config,
        }
    }

    pub(crate) fn ttl_for_key_class(&self, key_class: &str) -> Duration {
        if key_class == conduit_db::KEY_CLASS_PREVIOUS_RESPONSE_ID {
            self.config.response_continuity_ttl
        } else {
            self.config.prompt_cache_ttl
        }
    }

    pub(crate) async fn lookup(
        &self,
        key: &RouteAffinityKey,
    ) -> RepoResult<Option<RouteAffinityRow>> {
        let now = Utc::now();
        let cache_key = affinity_cache_key(key);
        match self.cache.get(&cache_key).await {
            Ok(Some(value)) => match serde_json::from_value::<RouteAffinityCacheEntry>(value) {
                Ok(entry) if entry.valid_until <= now => {}
                Ok(entry) => match entry.row {
                    Some(row) if row_matches_key(&row, key) && row.expires_at > now => {
                        return Ok(Some(row));
                    }
                    Some(_) => {
                        warn!("route affinity cache entry has the wrong scope");
                    }
                    None => return Ok(None),
                },
                Err(error) => {
                    warn!(%error, "route affinity cache entry is invalid");
                }
            },
            Ok(None) => {}
            Err(error) => {
                warn!(%error, "route affinity cache lookup failed; querying PostgreSQL");
            }
        }

        let ctx = system_context();
        let row = self
            .repo
            .find_valid_route_affinity_unchecked(&ctx, key, now)
            .await?;
        match row.as_ref() {
            Some(row) => self.cache_positive(&cache_key, row, now).await,
            None => {
                let Some(value) = route_cache_value(None, now, self.config.negative_cache_ttl)
                else {
                    warn!("route affinity negative-cache TTL is outside chrono range");
                    return Ok(row);
                };
                if let Err(error) = self
                    .cache
                    .set(&cache_key, value, Some(self.config.negative_cache_ttl))
                    .await
                {
                    warn!(%error, "failed to negative-cache route affinity lookup");
                }
            }
        }
        Ok(row)
    }

    pub(crate) async fn remember(
        &self,
        input: UpsertRouteAffinityInput,
    ) -> RepoResult<RouteAffinityRow> {
        let now = Utc::now();
        let cache_key = affinity_cache_key(&input.key);
        let row = self
            .repo
            .upsert_route_affinity_unchecked(&system_context(), input, now)
            .await?;
        self.cache_positive(&cache_key, &row, now).await;
        Ok(row)
    }

    async fn purge_expired(&self, limit: u32) -> RepoResult<u64> {
        self.repo
            .delete_expired_route_affinities_unchecked(&system_context(), Utc::now(), limit)
            .await
    }

    async fn cache_positive(&self, cache_key: &str, row: &RouteAffinityRow, now: DateTime<Utc>) {
        let remaining = (row.expires_at - now).to_std().unwrap_or_default();
        if remaining.is_zero() {
            return;
        }
        let ttl = remaining.min(self.config.lookup_cache_ttl);
        let Some(value) = route_cache_value(Some(row.clone()), now, ttl) else {
            warn!("route affinity lookup-cache TTL is outside chrono range");
            return;
        };
        if let Err(error) = self.cache.set(cache_key, value, Some(ttl)).await {
            warn!(%error, "failed to cache route affinity");
        }
    }
}

pub(crate) fn start_route_affinity_cleanup(runtime: Arc<RouteAffinityRuntime>) {
    tokio::spawn(async move {
        const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
        const CLEANUP_BATCH: u32 = 5_000;

        let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = runtime.purge_expired(CLEANUP_BATCH).await {
                warn!(%error, "failed to purge expired route affinities");
            }
        }
    });
}

pub(crate) fn hash_explicit_affinity_value(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub(crate) fn sticky_channel_cache_key(trace_id: &str) -> String {
    format!("last_channel:v2:{trace_id}")
}

pub(crate) fn sticky_channel_cache_value(
    channel_id: Option<String>,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Option<serde_json::Value> {
    let valid_until = cache_deadline(now, ttl)?;
    serde_json::to_value(StickyChannelCacheEntry {
        channel_id,
        valid_until,
    })
    .ok()
}

pub(crate) fn decode_sticky_channel_cache(
    value: serde_json::Value,
    now: DateTime<Utc>,
) -> Result<StickyChannelCacheState, serde_json::Error> {
    let entry = serde_json::from_value::<StickyChannelCacheEntry>(value)?;
    if entry.valid_until <= now {
        Ok(StickyChannelCacheState::Expired)
    } else {
        Ok(StickyChannelCacheState::Fresh(entry.channel_id))
    }
}

fn route_cache_value(
    row: Option<RouteAffinityRow>,
    now: DateTime<Utc>,
    ttl: Duration,
) -> Option<serde_json::Value> {
    let valid_until = cache_deadline(now, ttl)?;
    serde_json::to_value(RouteAffinityCacheEntry { row, valid_until }).ok()
}

fn cache_deadline(now: DateTime<Utc>, ttl: Duration) -> Option<DateTime<Utc>> {
    let ttl = chrono::Duration::from_std(ttl).ok()?;
    now.checked_add_signed(ttl)
}

fn affinity_cache_key(key: &RouteAffinityKey) -> String {
    let mut hasher = Sha256::new();
    for part in [
        key.project_id.as_str(),
        key.key_class.as_str(),
        key.key_hash.as_str(),
        key.public_model_id.as_str(),
        key.api_format.as_str(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("route_affinity:v2:{:x}", hasher.finalize())
}

fn row_matches_key(row: &RouteAffinityRow, key: &RouteAffinityKey) -> bool {
    row.project_id == key.project_id
        && row.key_class == key.key_class
        && row.key_hash == key.key_hash
        && row.public_model_id == key.public_model_id
        && row.api_format == key.api_format
}

fn system_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::{MemoryCache, TwoLevelCache};
    use conduit_db::{
        InMemoryRouteAffinityRepo, KEY_CLASS_PROMPT_CACHE_KEY, UpsertRouteAffinityInput,
    };

    fn config() -> RouteAffinityRuntimeConfig {
        RouteAffinityRuntimeConfig {
            prompt_cache_ttl: Duration::from_secs(3600),
            response_continuity_ttl: Duration::from_secs(7200),
            lookup_cache_ttl: Duration::from_secs(60),
            negative_cache_ttl: Duration::from_secs(5),
        }
    }

    fn key(hash: String) -> RouteAffinityKey {
        RouteAffinityKey {
            project_id: "7".into(),
            key_class: KEY_CLASS_PROMPT_CACHE_KEY.into(),
            key_hash: hash,
            public_model_id: "gpt-public".into(),
            api_format: "openai/responses".into(),
        }
    }

    #[test]
    fn hashes_and_cache_keys_never_embed_raw_affinity_values() {
        let raw = "customer-secret-cache-key";
        let key = key(hash_explicit_affinity_value(raw));
        let cache_key = affinity_cache_key(&key);

        assert_eq!(key.key_hash.len(), 64);
        assert!(!key.key_hash.contains(raw));
        assert!(!cache_key.contains(raw));
        assert!(!cache_key.contains("gpt-public"));
    }

    #[tokio::test]
    async fn remember_overwrites_negative_cache_and_lookup_returns_route() -> RepoResult<()> {
        let repo = Arc::new(InMemoryRouteAffinityRepo::new());
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new(Duration::from_secs(60)));
        let runtime = RouteAffinityRuntime::new(repo, cache, config());
        let key = key(hash_explicit_affinity_value("cache-key"));

        assert!(runtime.lookup(&key).await?.is_none());
        runtime
            .remember(UpsertRouteAffinityInput {
                key: key.clone(),
                channel_id: "11".into(),
                upstream_model_id: "gpt-upstream".into(),
                upstream_api_format: "openai/responses".into(),
                credential_identity: Some("sha256:credential".into()),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            })
            .await?;

        let found = runtime.lookup(&key).await?.expect("route affinity");
        assert_eq!(found.channel_id, "11");
        assert_eq!(
            found.credential_identity.as_deref(),
            Some("sha256:credential")
        );
        Ok(())
    }

    #[test]
    fn sticky_cache_envelope_expires_logically_even_if_backend_retains_it() -> Result<(), String> {
        let now = Utc::now();
        let value = sticky_channel_cache_value(Some("12".into()), now, Duration::from_secs(5))
            .ok_or_else(|| "cache value should serialize".to_string())?;

        assert_eq!(
            decode_sticky_channel_cache(value.clone(), now).map_err(|error| error.to_string())?,
            StickyChannelCacheState::Fresh(Some("12".into()))
        );
        assert_eq!(
            decode_sticky_channel_cache(value, now + chrono::Duration::seconds(6))
                .map_err(|error| error.to_string())?,
            StickyChannelCacheState::Expired
        );
        Ok(())
    }

    #[tokio::test]
    async fn two_level_l1_backfill_cannot_extend_negative_affinity_ttl() -> RepoResult<()> {
        let repo = Arc::new(InMemoryRouteAffinityRepo::new());
        let remote: Arc<dyn Cache> = Arc::new(MemoryCache::new(Duration::from_secs(300)));
        let writer_cache: Arc<dyn Cache> = Arc::new(TwoLevelCache::new(
            Arc::new(MemoryCache::new(Duration::from_secs(300))),
            remote.clone(),
        ));
        let reader_cache: Arc<dyn Cache> = Arc::new(TwoLevelCache::new(
            Arc::new(MemoryCache::new(Duration::from_secs(300))),
            remote,
        ));
        let config = RouteAffinityRuntimeConfig {
            negative_cache_ttl: Duration::from_millis(20),
            ..config()
        };
        let writer = RouteAffinityRuntime::new(repo.clone(), writer_cache, config);
        let reader = RouteAffinityRuntime::new(repo, reader_cache, config);
        let key = key(hash_explicit_affinity_value("cache-key"));

        assert!(writer.lookup(&key).await?.is_none());
        assert!(reader.lookup(&key).await?.is_none());
        writer
            .remember(UpsertRouteAffinityInput {
                key: key.clone(),
                channel_id: "11".into(),
                upstream_model_id: "gpt-upstream".into(),
                upstream_api_format: "openai/responses".into(),
                credential_identity: Some("sha256:credential".into()),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            })
            .await?;

        tokio::time::sleep(Duration::from_millis(40)).await;
        let found = reader
            .lookup(&key)
            .await?
            .expect("route after logical expiry");
        assert_eq!(found.channel_id, "11");
        Ok(())
    }
}

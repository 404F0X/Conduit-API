//! Generic cache entry mirroring Go `xcache.Entry[T]`
//! (`conduit/internal/pkg/xcache/entry.go:5-38`).
//!
//! The Entry layer adds two capabilities on top of the bare [`crate::Cache`]
//! trait:
//!   1. **Negative caching** — `is_empty = true` marks a "key not found"
//!      sentinel so subsequent reads can short-circuit without hitting the
//!      backing store (Go `project.go:213-219`).
//!   2. **Per-entry TTL** — `expire_at` records an absolute expiry instant.
//!      This is the **stale-while-revalidate gate**: even when the cache
//!      backend still holds the bytes, the service layer re-queries the
//!      backing store once [`CacheEntry::is_expired`] returns true
//!      (Go `project.go:192-204`).
//!
//! In Go the cache backend (gocache) has its *own* TTL on top of this
//! application-level expiry; the Rust [`crate::MemoryCache`] mirrors that
//! via its `set(..., ttl)` argument. `CacheEntry::expire_at` is therefore
//! the second, application-controlled layer, exactly as in Go.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Generic cache entry. Go parity: `conduit/internal/pkg/xcache/entry.go:7-11`.
///
/// The Go source has no explicit json tags on `Entry[T]`; the in-memory
/// backend (patrickmn/go-cache) stores the raw struct, while the Redis
/// backend uses a JSON codec that defaults to PascalCase. The entry is
/// internal cache plumbing — it is never exposed on the wire — so the Rust
/// JSON shape only needs to be self-consistent. We use the workspace
/// `camelCase` convention (CLAUDE.md parity rules) so a `CacheEntry` with
/// `expire_at = None` serializes to exactly `{"value":..., "isEmpty":...}`,
/// matching the existing service-local `ProjectCacheEntry` shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry<T> {
    /// Wrapped value. `None` for negative-cache markers; `Some(value)` for
    /// positive entries. Go stores a zero `T` for empty entries — the Rust
    /// `Option<T>` is the idiomatic equivalent and is what the existing
    /// service-layer mirrors already use. Note: serde auto-defaults a
    /// missing `Option<T>` field to `None`, so no `#[serde(default)]` is
    /// needed here (and adding one would wrongly require `T: Default`).
    pub value: Option<T>,

    /// `true` for negative-cache ("key not found") markers. Go field
    /// `Entry.IsEmpty` (entry.go:9).
    #[serde(default)]
    pub is_empty: bool,

    /// Absolute expiry instant. `None` mirrors Go's zero `time.Time`
    /// (`ExpireAt.IsZero()`), which means "never expires" per
    /// `Entry.IsExpired` (entry.go:14-16). As with `value`, serde
    /// auto-defaults a missing `Option<DateTime<Utc>>` to `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expire_at: Option<DateTime<Utc>>,
}

impl<T> CacheEntry<T> {
    /// Go parity: `Entry.IsExpired` (entry.go:14-16) —
    /// `!e.ExpireAt.IsZero() && time.Now().After(e.ExpireAt)`.
    ///
    /// Returns `true` only when `expire_at` is set and the current wall
    /// clock has passed it. A `None` expiry (Go zero `time.Time`) never
    /// expires.
    pub fn is_expired(&self) -> bool {
        match self.expire_at {
            Some(expire_at) => Utc::now() > expire_at,
            None => false,
        }
    }

    /// Go parity: `NewEntry[T](value, ttl)` (entry.go:19-30).
    ///
    /// A `ttl` of `None` or `Some(Duration::ZERO)` yields `expire_at = None`,
    /// matching Go's `ttl > 0` gate (entry.go:21) — the entry never expires
    /// at the application layer (the cache backend's own TTL still applies).
    pub fn new(value: T, ttl: Option<Duration>) -> Self {
        Self {
            value: Some(value),
            is_empty: false,
            expire_at: expiry_from_ttl(ttl),
        }
    }

    /// Go parity: `NewEmptyEntry[T](ttl)` (entry.go:33-38).
    ///
    /// Negative-cache marker: `is_empty = true`, `value = None`. Go always
    /// sets `ExpireAt = time.Now().Add(ttl)`; the Rust analogue treats
    /// `None`/zero ttl as "no application-layer expiry" so callers can
    /// encode either a bounded negative window (the common case,
    /// `negativeCacheTTL = 5s`) or a permanent marker.
    pub fn new_empty(ttl: Option<Duration>) -> Self {
        Self {
            value: None,
            is_empty: true,
            expire_at: expiry_from_ttl(ttl),
        }
    }
}

/// Convert a TTL `Duration` into an absolute expiry instant, mirroring Go's
/// `time.Now().Add(ttl)` when `ttl > 0` and returning `None` otherwise
/// (entry.go:21-23). A `Duration` larger than `chrono::Duration::MAX` is
/// treated as "never expires", matching Go's `time.Time` overflow behaviour
/// for practical TTLs.
fn expiry_from_ttl(ttl: Option<Duration>) -> Option<DateTime<Utc>> {
    let duration = ttl.filter(|duration| !duration.is_zero())?;
    let chrono_duration = chrono::Duration::from_std(duration).ok()?;
    Some(Utc::now() + chrono_duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheJsonExt, MemoryCache};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::time::Duration;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Payload {
        id: i64,
    }

    #[test]
    fn new_entry_with_zero_ttl_never_expires() {
        let entry = CacheEntry::new(Payload { id: 7 }, Some(Duration::ZERO));
        assert_eq!(entry.value, Some(Payload { id: 7 }));
        assert!(!entry.is_empty);
        assert_eq!(entry.expire_at, None);
        assert!(!entry.is_expired());
    }

    #[test]
    fn new_entry_with_none_ttl_never_expires() {
        let entry = CacheEntry::new(Payload { id: 9 }, None);
        assert_eq!(entry.expire_at, None);
        assert!(!entry.is_expired());
    }

    #[test]
    fn new_entry_with_future_ttl_is_not_expired() {
        let entry = CacheEntry::new(Payload { id: 1 }, Some(Duration::from_secs(60)));
        assert!(entry.expire_at.is_some());
        assert!(!entry.is_expired());
    }

    #[test]
    fn new_entry_with_past_ttl_is_expired() {
        // 1ns TTL — by the time we check it has already lapsed.
        let entry = CacheEntry::new(Payload { id: 2 }, Some(Duration::from_nanos(1)));
        // Spin until `Utc::now()` overtakes the recorded expiry. In practice
        // this is immediate, but we guard against a tight clock race.
        for _ in 0..16 {
            if entry.is_expired() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(entry.is_expired());
    }

    #[test]
    fn new_empty_marks_negative_cache_entry() {
        let entry = CacheEntry::<Payload>::new_empty(Some(Duration::from_secs(5)));
        assert!(entry.is_empty);
        assert_eq!(entry.value, None);
        assert!(entry.expire_at.is_some());
        assert!(!entry.is_expired());
    }

    #[test]
    fn new_empty_with_zero_ttl_has_no_expiry() {
        let entry = CacheEntry::<Payload>::new_empty(Some(Duration::ZERO));
        assert!(entry.is_empty);
        assert_eq!(entry.expire_at, None);
        assert!(!entry.is_expired());
    }

    #[test]
    fn json_round_trip_preserves_camel_case_and_omits_absent_expiry()
    -> Result<(), serde_json::Error> {
        let entry = CacheEntry::new(Payload { id: 42 }, None);
        let value = serde_json::to_value(&entry)?;
        // camelCase keys; expire_at omitted because of skip_serializing_if.
        assert_eq!(value, json!({ "value": { "id": 42 }, "isEmpty": false }));

        let empty = CacheEntry::<Payload>::new_empty(Some(Duration::from_secs(2)));
        let empty_value = serde_json::to_value(&empty)?;
        assert_eq!(
            empty_value
                .as_object()
                .map(|map| map.contains_key("expireAt")),
            Some(true)
        );
        assert_eq!(empty_value["isEmpty"], json!(true));
        Ok(())
    }

    #[test]
    fn json_deserializes_legacy_shape_without_expire_at() -> Result<(), serde_json::Error> {
        // Backwards-compatible with service-local entries that pre-date the
        // expire_at field (e.g. ProjectCacheEntry before the port).
        let value = json!({ "value": { "id": 5 }, "isEmpty": false });
        let entry: CacheEntry<Payload> = serde_json::from_value(value)?;
        assert_eq!(entry.value, Some(Payload { id: 5 }));
        assert!(!entry.is_empty);
        assert_eq!(entry.expire_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn typed_entry_round_trips_through_memory_cache() -> Result<(), Box<dyn std::error::Error>>
    {
        let cache = MemoryCache::new(Duration::from_secs(60));

        let entry = CacheEntry::new(Payload { id: 11 }, Some(Duration::from_secs(30)));
        cache.set_json("entry:1", &entry, None).await?;

        let loaded: CacheEntry<Payload> =
            cache.get_json("entry:1").await?.ok_or("entry missing")?;
        assert_eq!(loaded.value, Some(Payload { id: 11 }));
        assert!(!loaded.is_empty);
        assert!(!loaded.is_expired());
        Ok(())
    }

    #[tokio::test]
    async fn negative_entry_served_from_cache_until_expired()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go `project.go:192-204`: a negative marker is cached, then
        // served from cache until `is_expired` flips true.
        let cache = MemoryCache::new(Duration::from_secs(60));

        let negative = CacheEntry::<Payload>::new_empty(Some(Duration::from_millis(20)));
        cache.set_json("neg:1", &negative, None).await?;

        let first: CacheEntry<Payload> = cache.get_json("neg:1").await?.ok_or("missing")?;
        assert!(first.is_empty);
        assert!(!first.is_expired());

        tokio::time::sleep(Duration::from_millis(35)).await;

        let stale: CacheEntry<Payload> = cache.get_json("neg:1").await?.ok_or("missing")?;
        assert!(
            stale.is_expired(),
            "stale-while-revalidate gate must flip once expire_at passes"
        );
        Ok(())
    }

    #[tokio::test]
    async fn set_with_short_ttl_evicts_entry_from_backend() -> Result<(), Box<dyn std::error::Error>>
    {
        // Backend-layer TTL eviction (independent of CacheEntry::expire_at):
        // mirrors Go gocache's `store.WithExpiration` cleanup.
        let cache = MemoryCache::new(Duration::from_secs(60));

        cache
            .set_json(
                "ephemeral",
                &CacheEntry::new(Payload { id: 1 }, None),
                Some(Duration::from_millis(10)),
            )
            .await?;

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(
            cache.get_json::<CacheEntry<Payload>>("ephemeral").await?,
            None
        );
        Ok(())
    }
}

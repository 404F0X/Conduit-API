//! Repository contract for explicit provider route affinity.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::repo::{RepoError, RepoResult, RequestContext, guard_project_access};
use crate::row::RouteAffinityRow;

pub const KEY_CLASS_PREVIOUS_RESPONSE_ID: &str = "previous_response_id";
pub const KEY_CLASS_PROMPT_CACHE_KEY: &str = "prompt_cache_key";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteAffinityKey {
    pub project_id: String,
    pub key_class: String,
    pub key_hash: String,
    pub public_model_id: String,
    pub api_format: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertRouteAffinityInput {
    pub key: RouteAffinityKey,
    pub channel_id: String,
    pub upstream_model_id: String,
    pub upstream_api_format: String,
    pub credential_identity: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait RouteAffinityRepo: Send + Sync {
    async fn find_valid_route_affinity_unchecked(
        &self,
        ctx: &RequestContext,
        key: &RouteAffinityKey,
        now: DateTime<Utc>,
    ) -> RepoResult<Option<RouteAffinityRow>>;

    async fn upsert_route_affinity_unchecked(
        &self,
        ctx: &RequestContext,
        input: UpsertRouteAffinityInput,
        now: DateTime<Utc>,
    ) -> RepoResult<RouteAffinityRow>;

    async fn delete_expired_route_affinities_unchecked(
        &self,
        ctx: &RequestContext,
        now: DateTime<Utc>,
        limit: u32,
    ) -> RepoResult<u64>;

    async fn find_valid_route_affinity(
        &self,
        ctx: &RequestContext,
        key: &RouteAffinityKey,
        now: DateTime<Utc>,
    ) -> RepoResult<Option<RouteAffinityRow>> {
        guard_project_access(ctx, &key.project_id, crate::policy::ProjectAccess::Read)?;
        self.find_valid_route_affinity_unchecked(ctx, key, now)
            .await
    }

    async fn upsert_route_affinity(
        &self,
        ctx: &RequestContext,
        input: UpsertRouteAffinityInput,
        now: DateTime<Utc>,
    ) -> RepoResult<RouteAffinityRow> {
        guard_project_access(
            ctx,
            &input.key.project_id,
            crate::policy::ProjectAccess::Write,
        )?;
        self.upsert_route_affinity_unchecked(ctx, input, now).await
    }
}

#[derive(Debug, Default)]
pub struct InMemoryRouteAffinityRepo {
    rows: Mutex<BTreeMap<RouteAffinityKey, RouteAffinityRow>>,
}

impl InMemoryRouteAffinityRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("route affinity repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("route affinity repo"))?
            .is_empty())
    }
}

#[async_trait]
impl RouteAffinityRepo for InMemoryRouteAffinityRepo {
    async fn find_valid_route_affinity_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &RouteAffinityKey,
        now: DateTime<Utc>,
    ) -> RepoResult<Option<RouteAffinityRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("route affinity repo"))?
            .get(key)
            .filter(|row| row.expires_at > now)
            .cloned())
    }

    async fn upsert_route_affinity_unchecked(
        &self,
        _ctx: &RequestContext,
        input: UpsertRouteAffinityInput,
        now: DateTime<Utc>,
    ) -> RepoResult<RouteAffinityRow> {
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("route affinity repo"))?;
        let existing = rows.get(&input.key);
        let row = RouteAffinityRow {
            id: existing
                .map(|row| row.id.clone())
                .unwrap_or_else(|| (rows.len() + 1).to_string()),
            project_id: input.key.project_id.clone(),
            key_class: input.key.key_class.clone(),
            key_hash: input.key.key_hash.clone(),
            public_model_id: input.key.public_model_id.clone(),
            api_format: input.key.api_format.clone(),
            channel_id: input.channel_id,
            upstream_model_id: input.upstream_model_id,
            upstream_api_format: input.upstream_api_format,
            credential_identity: input.credential_identity,
            expires_at: input.expires_at,
            created_at: existing.map(|row| row.created_at).unwrap_or(now),
            updated_at: now,
        };
        rows.insert(input.key, row.clone());
        Ok(row)
    }

    async fn delete_expired_route_affinities_unchecked(
        &self,
        _ctx: &RequestContext,
        now: DateTime<Utc>,
        limit: u32,
    ) -> RepoResult<u64> {
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("route affinity repo"))?;
        let keys = rows
            .iter()
            .filter(|(_, row)| row.expires_at <= now)
            .take(limit as usize)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let deleted = keys.len() as u64;
        for key in keys {
            rows.remove(&key);
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyContext, Principal};
    use chrono::Duration;

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn key() -> RouteAffinityKey {
        RouteAffinityKey {
            project_id: "7".into(),
            key_class: KEY_CLASS_PROMPT_CACHE_KEY.into(),
            key_hash: "a".repeat(64),
            public_model_id: "gpt-public".into(),
            api_format: "openai/responses".into(),
        }
    }

    #[tokio::test]
    async fn upsert_replaces_route_without_changing_scope_identity() -> RepoResult<()> {
        let repo = InMemoryRouteAffinityRepo::new();
        let now = DateTime::parse_from_rfc3339("2026-08-24T00:00:00Z")
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_default();
        let input = |channel_id: &str, credential_identity: &str| UpsertRouteAffinityInput {
            key: key(),
            channel_id: channel_id.into(),
            upstream_model_id: "gpt-upstream".into(),
            upstream_api_format: "openai/responses".into(),
            credential_identity: Some(credential_identity.into()),
            expires_at: now + Duration::hours(1),
        };

        let first = repo
            .upsert_route_affinity(&context(), input("11", "sha256:first"), now)
            .await?;
        let second = repo
            .upsert_route_affinity(
                &context(),
                input("12", "sha256:second"),
                now + Duration::seconds(1),
            )
            .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(second.channel_id, "12");
        assert_eq!(second.credential_identity.as_deref(), Some("sha256:second"));
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn expired_affinity_is_not_returned() -> RepoResult<()> {
        let repo = InMemoryRouteAffinityRepo::new();
        let now = Utc::now();
        repo.upsert_route_affinity(
            &context(),
            UpsertRouteAffinityInput {
                key: key(),
                channel_id: "11".into(),
                upstream_model_id: "gpt-upstream".into(),
                upstream_api_format: "openai/responses".into(),
                credential_identity: None,
                expires_at: now,
            },
            now,
        )
        .await?;

        assert!(
            repo.find_valid_route_affinity(&context(), &key(), now)
                .await?
                .is_none()
        );
        assert_eq!(
            repo.delete_expired_route_affinities_unchecked(&context(), now, 100)
                .await?,
            1
        );
        assert_eq!(repo.len()?, 0);
        Ok(())
    }
}

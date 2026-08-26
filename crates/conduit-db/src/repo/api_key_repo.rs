//! API-key repository — Rust port of `conduit/internal/server/biz/api_key.go`.
//!
//! Mirrors the Go `APIKeyService` data-access surface against the `APIKey` Ent
//! schema (`internal/ent/schema/api_key.go`): `key`-keyed lookup (cache-shaped),
//! per-project listing, status/profiles/scopes mutations, soft delete, and
//! paginated listing.
//!
//! ## Storage model (RUST-P3-002 S13)
//! `ApiKeyRow` is now a hand-written typed struct carrying the real Go entity
//! columns (key, name, type, status, scopes, profiles, user_id, project_id,
//! timestamps, deleted_at).
//!
//! ## Uniqueness (differs from UserRepo/ProjectRepo)
//! Go declares `key` as a **single-column** unique index (`api_keys_by_key`).
//! Unlike the `(email, deleted_at)` / `(name, deleted_at)` composite semantics,
//! a soft-deleted api-key row **does not** free its `key` for re-registration.
//! So `key` conflict is checked against **all** rows (live and soft-deleted).
//!
//! `name` has **no DB unique constraint**; per-project name uniqueness is
//! enforced at the application level (live rows only, reusable after soft delete).
//!
//! ## Soft delete (Go parity: status is NOT touched)
//! Go's `SoftDeleteMixin` sets ONLY `deleted_at` and leaves `status` untouched.

use crate::repo::{
    RepoError, RepoResult, RequestContext, guard_project_access, guard_repo_principal,
};
use crate::row::ApiKeyRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::policy::ProjectAccess;

/// Fields a caller may set when creating an api-key.
#[derive(Debug, Clone)]
pub struct CreateApiKeyInput {
    pub id: String,
    pub user_id: Option<String>,
    pub project_id: String,
    pub name: String,
    pub key: String,
    pub key_type: String,
    pub scopes: Vec<String>,
    pub profiles: Option<Value>,
    pub created_at: String,
}

/// Patch applied by `update_api_key`. Only non-`None` fields are written.
#[derive(Debug, Default, Clone)]
pub struct UpdateApiKeyInput {
    pub name: Option<String>,
    pub key_type: Option<String>,
    pub status: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub profiles: Option<Value>,
    pub updated_at: String,
}

/// Pagination/filter params for `list_api_keys`.
#[derive(Debug, Clone, Default)]
pub struct ListApiKeysQuery {
    pub limit: u32,
    pub offset: u32,
    pub project_id: Option<String>,
    pub user_id: Option<String>,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl ListApiKeysQuery {
    pub fn for_project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: Some(project_id.into()),
            limit: 20,
            ..Default::default()
        }
    }

    pub fn for_user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: Some(user_id.into()),
            limit: 20,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListApiKeysResult {
    pub rows: Vec<ApiKeyRow>,
    pub has_more: bool,
}

// --- timestamp parsing -----------------------------------------------------

fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| {
                DateTime::<Utc>::from_naive_utc_and_offset(
                    d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    Utc,
                )
            })
        })
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

// --- row helpers -----------------------------------------------------------

fn is_live(row: &ApiKeyRow) -> bool {
    row.deleted_at.is_none()
}

fn row_from_input(input: &CreateApiKeyInput) -> ApiKeyRow {
    let now = parse_dt(&input.created_at);
    ApiKeyRow {
        id: input.id.clone(),
        name: input.name.clone(),
        status: "enabled".into(),
        project_id: input.project_id.clone(),
        user_id: input.user_id.clone(),
        key: input.key.clone(),
        key_type: input.key_type.clone(),
        scopes: input.scopes.clone(),
        profiles: input
            .profiles
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default())),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut ApiKeyRow, input: &UpdateApiKeyInput) {
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(key_type) = &input.key_type {
        row.key_type = key_type.clone();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    if let Some(scopes) = &input.scopes {
        row.scopes = scopes.clone();
    }
    if let Some(profiles) = &input.profiles {
        row.profiles = profiles.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait ApiKeyRepo: Send + Sync {
    async fn create_api_key_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateApiKeyInput,
    ) -> RepoResult<ApiKeyRow>;

    async fn create_api_key(
        &self,
        ctx: &RequestContext,
        input: CreateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        self.create_api_key_unchecked(ctx, input).await
    }

    async fn find_api_key_by_id_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>>;

    async fn find_api_key_by_id(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        guard_repo_principal(ctx)?;
        self.find_api_key_by_id_unchecked(ctx, api_key_id).await
    }

    async fn find_api_key_by_id_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>>;

    async fn find_api_key_by_id_with_deleted(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        guard_repo_principal(ctx)?;
        self.find_api_key_by_id_with_deleted_unchecked(ctx, api_key_id)
            .await
    }

    async fn find_api_key_by_key_unchecked(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>>;

    async fn find_api_key_by_key(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        guard_repo_principal(ctx)?;
        self.find_api_key_by_key_unchecked(ctx, key).await
    }

    async fn find_api_key_by_key_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>>;

    async fn find_api_key_by_key_with_deleted(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        guard_repo_principal(ctx)?;
        self.find_api_key_by_key_with_deleted_unchecked(ctx, key)
            .await
    }

    async fn list_api_keys_by_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult>;

    async fn list_api_keys_by_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        guard_repo_principal(ctx)?;
        self.list_api_keys_by_user_unchecked(ctx, user_id, query)
            .await
    }

    async fn list_api_keys_by_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult>;

    async fn list_api_keys_by_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.list_api_keys_by_project_unchecked(ctx, project_id, query)
            .await
    }

    async fn list_api_keys_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult>;

    async fn list_api_keys(
        &self,
        ctx: &RequestContext,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        guard_repo_principal(ctx)?;
        self.list_api_keys_unchecked(ctx, query).await
    }

    async fn update_api_key_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        input: UpdateApiKeyInput,
    ) -> RepoResult<ApiKeyRow>;

    async fn rotate_api_key_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        new_key: &str,
    ) -> RepoResult<ApiKeyRow>;

    async fn rotate_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        new_key: &str,
    ) -> RepoResult<ApiKeyRow> {
        guard_repo_principal(ctx)?;
        self.rotate_api_key_unchecked(ctx, api_key_id, new_key)
            .await
    }

    async fn update_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        input: UpdateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        guard_repo_principal(ctx)?;
        self.update_api_key_unchecked(ctx, api_key_id, input).await
    }

    async fn soft_delete_api_key_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ApiKeyRow>;

    async fn soft_delete_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ApiKeyRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_api_key_unchecked(ctx, api_key_id, deleted_at)
            .await
    }

    async fn restore_api_key_unchecked(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<ApiKeyRow>;

    async fn restore_api_key(
        &self,
        ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<ApiKeyRow> {
        guard_repo_principal(ctx)?;
        self.restore_api_key_unchecked(ctx, api_key_id).await
    }

    async fn api_key_exists_unchecked(&self, ctx: &RequestContext, key: &str) -> RepoResult<bool>;

    async fn api_key_exists(&self, ctx: &RequestContext, key: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.api_key_exists_unchecked(ctx, key).await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryApiKeyRepo {
    rows: Mutex<BTreeMap<String, ApiKeyRow>>,
}

impl InMemoryApiKeyRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = ApiKeyRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    /// Key uniqueness spans ALL rows (live + soft-deleted), matching Go's
    /// single-column `api_keys_by_key` unique index.
    fn key_in_use_locked(
        guard: &BTreeMap<String, ApiKeyRow>,
        key: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| row.key == key && Some(row.id.as_str()) != exclude_id)
    }

    /// Per-project name uniqueness (live rows only).
    fn name_in_use_locked(
        guard: &BTreeMap<String, ApiKeyRow>,
        project_id: &str,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard.values().any(|row| {
            is_live(row)
                && row.project_id == project_id
                && row.name == name
                && Some(row.id.as_str()) != exclude_id
        })
    }
}

#[async_trait]
impl ApiKeyRepo for InMemoryApiKeyRepo {
    async fn create_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        if Self::key_in_use_locked(&guard, &input.key, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("api_key id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_api_key_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .get(api_key_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_api_key_by_id_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .get(api_key_id)
            .cloned())
    }

    async fn find_api_key_by_key_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .values()
            .find(|r| is_live(r) && r.key == key)
            .cloned())
    }

    async fn find_api_key_by_key_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .values()
            .find(|r| r.key == key)
            .cloned())
    }

    async fn list_api_keys_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        let mut live: Vec<ApiKeyRow> = guard
            .values()
            .filter(|r| is_live(r) && r.user_id.as_deref() == Some(user_id))
            .cloned()
            .collect();
        sort_and_paginate(&mut live, query)
    }

    async fn list_api_keys_by_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        let mut live: Vec<ApiKeyRow> = guard
            .values()
            .filter(|r| is_live(r) && r.project_id == project_id)
            .cloned()
            .collect();
        sort_and_paginate(&mut live, query)
    }

    async fn list_api_keys_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        let mut live: Vec<ApiKeyRow> = guard
            .values()
            .filter(|r| {
                if !is_live(r) {
                    return false;
                }
                if let Some(pid) = &query.project_id
                    && r.project_id != *pid
                {
                    return false;
                }
                if let Some(uid) = &query.user_id
                    && r.user_id.as_deref() != Some(uid.as_str())
                {
                    return false;
                }
                true
            })
            .cloned()
            .collect();
        sort_and_paginate(&mut live, query)
    }

    async fn update_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
        input: UpdateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;

        if let Some(new_name) = &input.name {
            let project_id = guard
                .get(api_key_id)
                .filter(|r| is_live(r))
                .map(|r| r.project_id.clone())
                .ok_or(RepoError::NotFound("api_key"))?;
            if Self::name_in_use_locked(&guard, &project_id, new_name, Some(api_key_id)) {
                return Err(RepoError::NameConflict);
            }
        }

        let row = guard
            .get_mut(api_key_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("api_key"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn rotate_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
        new_key: &str,
    ) -> RepoResult<ApiKeyRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        if guard.values().any(|row| row.key == new_key) {
            return Err(RepoError::NameConflict);
        }
        let row = guard
            .get_mut(api_key_id)
            .filter(|row| is_live(row))
            .ok_or(RepoError::NotFound("api_key"))?;
        row.key = new_key.to_string();
        Ok(row.clone())
    }

    async fn soft_delete_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ApiKeyRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;
        let row = guard
            .get_mut(api_key_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("api_key"))?;
        // Go parity: status is NOT touched on soft delete.
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        Ok(row.clone())
    }

    async fn restore_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
    ) -> RepoResult<ApiKeyRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?;

        let is_deleted = guard
            .get(api_key_id)
            .map(|r| r.deleted_at.is_some())
            .ok_or(RepoError::NotFound("api_key"))?;

        if is_deleted {
            let row = guard
                .get_mut(api_key_id)
                .ok_or(RepoError::NotFound("api_key"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "enabled".into();
        }
        Ok(guard
            .get(api_key_id)
            .ok_or(RepoError::NotFound("api_key"))?
            .clone())
    }

    async fn api_key_exists_unchecked(&self, _ctx: &RequestContext, key: &str) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("api_key repo"))?
            .values()
            .any(|r| r.key == key))
    }
}

// --- shared pagination helper ----------------------------------------------

fn sort_and_paginate(
    rows: &mut Vec<ApiKeyRow>,
    query: &ListApiKeysQuery,
) -> RepoResult<ListApiKeysResult> {
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    if let (Some(cursor_ts), Some(cursor_id)) =
        (query.after_created_at.as_deref(), query.after_id.as_deref())
    {
        let cursor_dt = parse_dt(cursor_ts);
        rows.retain(|r| {
            r.created_at
                .cmp(&cursor_dt)
                .then_with(|| r.id.as_str().cmp(cursor_id))
                == std::cmp::Ordering::Greater
        });
    }

    let limit = query.limit as usize;
    let offset = query.offset as usize;
    let window_start = offset.min(rows.len());
    let window_end = (window_start + limit).min(rows.len());
    let out = rows[window_start..window_end].to_vec();
    let has_more = window_end < rows.len();

    Ok(ListApiKeysResult {
        rows: out,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn input(
        id: &str,
        key: &str,
        name: &str,
        project: &str,
        created_at: &str,
    ) -> CreateApiKeyInput {
        CreateApiKeyInput {
            id: id.into(),
            user_id: Some("u-1".into()),
            project_id: project.into(),
            name: name.into(),
            key: key.into(),
            key_type: "user".into(),
            scopes: vec!["read_channels".into(), "write_requests".into()],
            profiles: None,
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id_and_key() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_api_key(
                &ctx,
                input("k-1", "sk-secret-1", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
            )
            .await?;
        assert_eq!(created.id, "k-1");
        assert_eq!(created.name, "Alpha");
        assert_eq!(created.status, "enabled");
        assert_eq!(created.key, "sk-secret-1");

        let by_id = repo
            .find_api_key_by_id(&ctx, "k-1")
            .await?
            .ok_or(RepoError::NotFound("k-1"))?;
        assert_eq!(by_id.id, "k-1");

        let by_key = repo
            .find_api_key_by_key(&ctx, "sk-secret-1")
            .await?
            .ok_or(RepoError::NotFound("key"))?;
        assert_eq!(by_key.id, "k-1");
        Ok(())
    }

    #[tokio::test]
    async fn find_by_key_miss_returns_none() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();
        repo.create_api_key(
            &ctx,
            input("k-1", "sk-real", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        assert!(
            repo.find_api_key_by_key(&ctx, "sk-missing")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn key_conflict_on_create() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();
        repo.create_api_key(
            &ctx,
            input("k-1", "sk-dup", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        let dup = repo
            .create_api_key(
                &ctx,
                input("k-2", "sk-dup", "Beta", "p-2", "2024-01-02T00:00:00Z"),
            )
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_then_restore_round_trip() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();
        repo.create_api_key(
            &ctx,
            input("k-1", "sk-1", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        let deleted = repo
            .soft_delete_api_key(&ctx, "k-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "enabled");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_api_key_by_id(&ctx, "k-1").await?.is_none());
        assert!(repo.find_api_key_by_key(&ctx, "sk-1").await?.is_none());
        assert!(
            repo.find_api_key_by_id_with_deleted(&ctx, "k-1")
                .await?
                .is_some()
        );
        assert!(
            repo.find_api_key_by_key_with_deleted(&ctx, "sk-1")
                .await?
                .is_some()
        );

        let restored = repo.restore_api_key(&ctx, "k-1").await?;
        assert_eq!(restored.status, "enabled");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_api_key_by_key(&ctx, "sk-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn by_project_filters_and_paginates() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_api_key(&ctx, input("k-1", "sk-1", "A", "p-1", ts))
            .await?;
        repo.create_api_key(&ctx, input("k-2", "sk-2", "B", "p-1", ts))
            .await?;
        repo.create_api_key(&ctx, input("k-3", "sk-3", "C", "p-2", ts))
            .await?;

        let p1 = repo
            .list_api_keys_by_project(
                &ctx,
                "p-1",
                &ListApiKeysQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = p1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["k-1", "k-2"]);

        let mine = repo
            .list_api_keys_by_user(&ctx, "u-1", &ListApiKeysQuery::for_user("u-1"))
            .await?;
        assert_eq!(mine.rows.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn keyset_pagination_after_cursor() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();
        repo.create_api_key(&ctx, input("a", "sk-a", "A", "p-1", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_api_key(&ctx, input("b", "sk-b", "B", "p-1", "2024-01-02T00:00:00Z"))
            .await?;
        repo.create_api_key(&ctx, input("c", "sk-c", "C", "p-1", "2024-01-03T00:00:00Z"))
            .await?;

        let next = repo
            .list_api_keys(
                &ctx,
                &ListApiKeysQuery {
                    limit: 10,
                    after_created_at: Some("2024-01-02T00:00:00Z".into()),
                    after_id: Some("b".into()),
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = next.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["c"]);
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_api_key_by_id(&anon, "k-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_by_key = repo.find_api_key_by_key(&anon, "sk-x").await;
        assert!(matches!(denied_by_key, Err(RepoError::Policy(_))));
        Ok(())
    }

    #[tokio::test]
    async fn s15_crud_lifecycle_round_trips_and_project_filter_is_in_method_name() -> RepoResult<()>
    {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_api_key(
                &ctx,
                input("k-1", "sk-1", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
            )
            .await?;
        assert_eq!(created.id, "k-1");

        let fetched = repo
            .find_api_key_by_id(&ctx, "k-1")
            .await?
            .ok_or(RepoError::NotFound("k-1"))?;
        assert_eq!(fetched.id, "k-1");

        assert!(repo.api_key_exists(&ctx, "sk-1").await?);
        assert!(!repo.api_key_exists(&ctx, "sk-missing").await?);

        repo.create_api_key(
            &ctx,
            input("k-2", "sk-2", "Beta", "p-1", "2024-01-02T00:00:00Z"),
        )
        .await?;
        repo.create_api_key(
            &ctx,
            input("k-3", "sk-3", "Gamma", "p-2", "2024-01-03T00:00:00Z"),
        )
        .await?;

        let p1_rows = repo
            .list_api_keys_by_project(&ctx, "p-1", &ListApiKeysQuery::for_project("p-1"))
            .await?;
        let p1_ids: Vec<_> = p1_rows.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(p1_ids, vec!["k-1", "k-2"]);

        let generic_p1 = repo
            .list_api_keys(
                &ctx,
                &ListApiKeysQuery {
                    project_id: Some("p-1".into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        let generic_ids: Vec<_> = generic_p1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(generic_ids, vec!["k-1", "k-2"]);

        let updated = repo
            .update_api_key(
                &ctx,
                "k-1",
                UpdateApiKeyInput {
                    name: Some("Alpha-Renamed".into()),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.name, "Alpha-Renamed");

        let deleted = repo
            .soft_delete_api_key(&ctx, "k-1", "2024-03-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "enabled");
        assert!(
            repo.find_api_key_by_id(&ctx, "k-1").await?.is_none(),
            "soft-deleted row must be invisible to the default finder"
        );

        let restored = repo.restore_api_key(&ctx, "k-1").await?;
        assert_eq!(restored.status, "enabled");
        assert!(
            repo.find_api_key_by_id(&ctx, "k-1").await?.is_some(),
            "restored row must reappear in the default finder"
        );
        Ok(())
    }

    #[tokio::test]
    async fn s15_with_deleted_variant_bypasses_default_soft_delete_filter() -> RepoResult<()> {
        let repo = InMemoryApiKeyRepo::new();
        let ctx = ctx_allowed();

        repo.create_api_key(
            &ctx,
            input("k-1", "sk-1", "Alpha", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.soft_delete_api_key(&ctx, "k-1", "2024-02-01T00:00:00Z")
            .await?;

        assert!(
            repo.find_api_key_by_id(&ctx, "k-1").await?.is_none(),
            "default finder must hide soft-deleted rows"
        );
        assert!(
            repo.find_api_key_by_id_with_deleted(&ctx, "k-1")
                .await?
                .is_some(),
            "*_with_deleted variant must see soft-deleted rows"
        );
        assert!(
            repo.find_api_key_by_key_with_deleted(&ctx, "sk-1")
                .await?
                .is_some(),
            "key-shaped *_with_deleted variant must see soft-deleted rows"
        );
        Ok(())
    }
}

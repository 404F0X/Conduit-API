//! Data-storage repository — Rust port of `conduit/internal/server/biz/data_storage.go`.
//!
//! RUST-P3-002 S13: `DataStorageRow` is now a hand-written typed struct.
//!
//! ## Uniqueness (mirrors Go `DataStorage.Indexes`)
//! The Go schema declares `data_sources_by_name` on `name` **alone** (no
//! `deleted_at`), so a soft-deleted row does NOT free its `name`. The InMemory
//! impl checks live rows only — a known divergence; the sqlx impl matches the
//! real DB constraint.
//!
//! ## Status semantics
//! Go enum: `active | archived`; default `active`. Soft delete sets `archived`.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::DataStorageRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when creating a data storage.
#[derive(Debug, Clone)]
pub struct CreateDataStorageInput {
    pub id: String,
    pub name: String,
    pub description: String,
    pub primary: bool,
    pub storage_type: Option<String>,
    pub settings: Option<Value>,
    pub created_at: String,
}

impl CreateDataStorageInput {
    /// Mirrors `CreateDataStorage.SetPrimary(false)` — the public API never
    /// sets primary; this is the conventional construction path.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        storage_type: impl Into<String>,
        settings: Value,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            primary: false,
            storage_type: Some(storage_type.into()),
            settings: Some(settings),
            created_at: created_at.into(),
        }
    }
}

/// Patch applied by `update_data_storage`. `primary` is intentionally absent
/// (Go marks it `Immutable()`).
#[derive(Debug, Default, Clone)]
pub struct UpdateDataStorageInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub storage_type: Option<String>,
    pub settings: Option<Value>,
    pub status: Option<String>,
    pub updated_at: String,
}

/// Pagination/filter params for `list_data_storages`.
#[derive(Debug, Clone)]
pub struct ListDataStoragesQuery {
    pub limit: u32,
    pub offset: u32,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl Default for ListDataStoragesQuery {
    fn default() -> Self {
        Self {
            limit: 20,
            offset: 0,
            after_created_at: None,
            after_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListDataStoragesResult {
    pub rows: Vec<DataStorageRow>,
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

fn is_live(row: &DataStorageRow) -> bool {
    row.deleted_at.is_none()
}

fn row_from_input(input: &CreateDataStorageInput) -> DataStorageRow {
    let now = parse_dt(&input.created_at);
    DataStorageRow {
        id: input.id.clone(),
        name: input.name.clone(),
        status: "active".into(),
        description: input.description.clone(),
        primary: input.primary,
        storage_type: input
            .storage_type
            .clone()
            .unwrap_or_else(|| "database".into()),
        settings: input.settings.clone().unwrap_or_default(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut DataStorageRow, input: &UpdateDataStorageInput) {
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(description) = &input.description {
        row.description = description.clone();
    }
    if let Some(storage_type) = &input.storage_type {
        row.storage_type = storage_type.clone();
    }
    if let Some(settings) = &input.settings {
        row.settings = settings.clone();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait DataStorageRepo: Send + Sync {
    async fn create_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateDataStorageInput,
    ) -> RepoResult<DataStorageRow>;

    async fn create_data_storage(
        &self,
        ctx: &RequestContext,
        input: CreateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        guard_repo_principal(ctx)?;
        self.create_data_storage_unchecked(ctx, input).await
    }

    async fn find_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>>;

    async fn find_data_storage(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        guard_repo_principal(ctx)?;
        self.find_data_storage_unchecked(ctx, storage_id).await
    }

    async fn find_data_storage_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>>;

    async fn find_data_storage_with_deleted(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        guard_repo_principal(ctx)?;
        self.find_data_storage_with_deleted_unchecked(ctx, storage_id)
            .await
    }

    async fn find_data_storage_by_name_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<DataStorageRow>>;

    async fn find_data_storage_by_name(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        guard_repo_principal(ctx)?;
        self.find_data_storage_by_name_unchecked(ctx, name).await
    }

    async fn find_primary_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Option<DataStorageRow>>;

    async fn find_primary_data_storage(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Option<DataStorageRow>> {
        guard_repo_principal(ctx)?;
        self.find_primary_data_storage_unchecked(ctx).await
    }

    async fn list_data_storages_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListDataStoragesQuery,
    ) -> RepoResult<ListDataStoragesResult>;

    async fn list_data_storages(
        &self,
        ctx: &RequestContext,
        query: &ListDataStoragesQuery,
    ) -> RepoResult<ListDataStoragesResult> {
        guard_repo_principal(ctx)?;
        self.list_data_storages_unchecked(ctx, query).await
    }

    async fn update_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        input: UpdateDataStorageInput,
    ) -> RepoResult<DataStorageRow>;

    async fn update_data_storage(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        input: UpdateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        guard_repo_principal(ctx)?;
        self.update_data_storage_unchecked(ctx, storage_id, input)
            .await
    }

    async fn soft_delete_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        deleted_at: &str,
    ) -> RepoResult<DataStorageRow>;

    async fn soft_delete_data_storage(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        deleted_at: &str,
    ) -> RepoResult<DataStorageRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_data_storage_unchecked(ctx, storage_id, deleted_at)
            .await
    }

    async fn restore_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<DataStorageRow>;

    async fn restore_data_storage(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<DataStorageRow> {
        guard_repo_principal(ctx)?;
        self.restore_data_storage_unchecked(ctx, storage_id).await
    }

    async fn data_storage_exists_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool>;

    async fn data_storage_exists(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.data_storage_exists_unchecked(ctx, name).await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryDataStorageRepo {
    rows: Mutex<BTreeMap<String, DataStorageRow>>,
}

impl InMemoryDataStorageRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = DataStorageRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    /// Live-rows-only name check (InMemory divergence from the real DB; the
    /// real `data_sources_by_name` index spans all rows).
    fn name_in_use_locked(
        guard: &BTreeMap<String, DataStorageRow>,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| is_live(row) && row.name == name && Some(row.id.as_str()) != exclude_id)
    }
}

#[async_trait]
impl DataStorageRepo for InMemoryDataStorageRepo {
    async fn create_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?;
        if Self::name_in_use_locked(&guard, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("data storage id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .get(storage_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_data_storage_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .get(storage_id)
            .cloned())
    }

    async fn find_data_storage_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .values()
            .find(|r| is_live(r) && r.name == name)
            .cloned())
    }

    async fn find_primary_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Option<DataStorageRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .values()
            .find(|r| is_live(r) && r.primary)
            .cloned())
    }

    async fn list_data_storages_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListDataStoragesQuery,
    ) -> RepoResult<ListDataStoragesResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?;

        let mut live: Vec<DataStorageRow> =
            guard.values().filter(|r| is_live(r)).cloned().collect();
        live.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        if let (Some(cursor_ts), Some(cursor_id)) =
            (query.after_created_at.as_deref(), query.after_id.as_deref())
        {
            let cursor_dt = parse_dt(cursor_ts);
            live.retain(|r| {
                r.created_at
                    .cmp(&cursor_dt)
                    .then_with(|| r.id.as_str().cmp(cursor_id))
                    == std::cmp::Ordering::Greater
            });
        }

        let limit = query.limit as usize;
        let offset = query.offset as usize;
        let window_start = offset.min(live.len());
        let window_end = (window_start + limit).min(live.len());
        let rows = live[window_start..window_end].to_vec();
        let has_more = window_end < live.len();

        Ok(ListDataStoragesResult { rows, has_more })
    }

    async fn update_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
        input: UpdateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?;

        if let Some(new_name) = &input.name
            && Self::name_in_use_locked(&guard, new_name, Some(storage_id))
        {
            return Err(RepoError::NameConflict);
        }

        let row = guard
            .get_mut(storage_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("data storage"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
        deleted_at: &str,
    ) -> RepoResult<DataStorageRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?;
        let row = guard
            .get_mut(storage_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("data storage"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        row.status = "archived".into();
        Ok(row.clone())
    }

    async fn restore_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<DataStorageRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?;

        let (name, is_deleted) = guard
            .get(storage_id)
            .map(|r| (r.name.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("data storage"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, &name, Some(storage_id)) {
                return Err(RepoError::NameConflict);
            }
            let row = guard
                .get_mut(storage_id)
                .ok_or(RepoError::NotFound("data storage"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "active".into();
        }
        Ok(guard
            .get(storage_id)
            .ok_or(RepoError::NotFound("data storage"))?
            .clone())
    }

    async fn data_storage_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("data storage repo"))?
            .values()
            .any(|r| is_live(r) && r.name == name))
    }
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

    fn input(id: &str, name: &str, created_at: &str) -> CreateDataStorageInput {
        CreateDataStorageInput::new(
            id,
            name,
            "demo",
            "database",
            serde_json::json!({"dsn": ":memory:"}),
            created_at,
        )
    }

    fn input_primary(id: &str, name: &str, created_at: &str) -> CreateDataStorageInput {
        let mut i = input(id, name, created_at);
        i.primary = true;
        i
    }

    #[tokio::test]
    async fn create_then_find_by_id_and_name() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_data_storage(&ctx, input("ds-1", "Primary DB", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "ds-1");
        assert_eq!(created.name, "Primary DB");
        assert_eq!(created.status, "active");
        assert!(!created.primary);

        let by_id = repo
            .find_data_storage(&ctx, "ds-1")
            .await?
            .ok_or(RepoError::NotFound("ds-1"))?;
        assert_eq!(by_id.id, "ds-1");

        let by_name = repo
            .find_data_storage_by_name(&ctx, "Primary DB")
            .await?
            .ok_or(RepoError::NotFound("name"))?;
        assert_eq!(by_name.id, "ds-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_data_storage(&anon, "ds-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_data_storage(&anon, input("ds-2", "Beta", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn name_conflict_on_create_and_update() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();
        repo.create_data_storage(&ctx, input("ds-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_data_storage(&ctx, input("ds-2", "Beta", "2024-01-02T00:00:00Z"))
            .await?;

        let dup = repo
            .create_data_storage(&ctx, input("ds-3", "Alpha", "2024-01-03T00:00:00Z"))
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));

        let rename = repo
            .update_data_storage(
                &ctx,
                "ds-2",
                UpdateDataStorageInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-04T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::NameConflict)));

        let self_rename = repo
            .update_data_storage(
                &ctx,
                "ds-1",
                UpdateDataStorageInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-05T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(self_rename.name, "Alpha");
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_hides_row_from_default_queries() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();
        repo.create_data_storage(&ctx, input("ds-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let deleted = repo
            .soft_delete_data_storage(&ctx, "ds-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "archived");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_data_storage(&ctx, "ds-1").await?.is_none());
        assert!(
            repo.find_data_storage_by_name(&ctx, "Alpha")
                .await?
                .is_none()
        );
        assert!(!repo.data_storage_exists(&ctx, "Alpha").await?);
        assert!(
            repo.find_data_storage_with_deleted(&ctx, "ds-1")
                .await?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn restore_brings_row_back_and_frees_name() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();
        repo.create_data_storage(&ctx, input("ds-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.soft_delete_data_storage(&ctx, "ds-1", "2024-02-01T00:00:00Z")
            .await?;

        repo.create_data_storage(&ctx, input("ds-2", "Alpha", "2024-03-01T00:00:00Z"))
            .await?;

        let restore_conflict = repo.restore_data_storage(&ctx, "ds-1").await;
        assert!(matches!(restore_conflict, Err(RepoError::NameConflict)));

        repo.soft_delete_data_storage(&ctx, "ds-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_data_storage(&ctx, "ds-1").await?;
        assert_eq!(restored.status, "active");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_data_storage(&ctx, "ds-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn find_primary_returns_the_flagged_live_row() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();

        assert!(repo.find_primary_data_storage(&ctx).await?.is_none());

        repo.create_data_storage(&ctx, input("ds-1", "Replica", "2024-01-01T00:00:00Z"))
            .await?;
        assert!(repo.find_primary_data_storage(&ctx).await?.is_none());

        repo.create_data_storage(
            &ctx,
            input_primary("ds-0", "System DB", "2024-01-01T00:00:00Z"),
        )
        .await?;
        let primary = repo
            .find_primary_data_storage(&ctx)
            .await?
            .ok_or(RepoError::NotFound("primary"))?;
        assert_eq!(primary.id, "ds-0");
        assert!(primary.primary);

        repo.soft_delete_data_storage(&ctx, "ds-0", "2024-02-01T00:00:00Z")
            .await?;
        assert!(repo.find_primary_data_storage(&ctx).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_data_storage(&ctx, input("c", "C", ts)).await?;
        repo.create_data_storage(&ctx, input("a", "A", ts)).await?;
        repo.create_data_storage(&ctx, input("b", "B", ts)).await?;

        let page1 = repo
            .list_data_storages(
                &ctx,
                &ListDataStoragesQuery {
                    limit: 2,
                    offset: 0,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(page1.has_more);

        let page2 = repo
            .list_data_storages(
                &ctx,
                &ListDataStoragesQuery {
                    limit: 2,
                    offset: 2,
                    ..Default::default()
                },
            )
            .await?;
        let ids2: Vec<_> = page2.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids2, vec!["c"]);
        assert!(!page2.has_more);
        Ok(())
    }

    #[tokio::test]
    async fn update_settings_and_status() -> RepoResult<()> {
        let repo = InMemoryDataStorageRepo::new();
        let ctx = ctx_allowed();
        repo.create_data_storage(&ctx, input("ds-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let updated = repo
            .update_data_storage(
                &ctx,
                "ds-1",
                UpdateDataStorageInput {
                    status: Some("archived".into()),
                    settings: Some(serde_json::json!({"dsn": ":memory:v2:"})),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, "archived");
        assert_eq!(updated.settings, serde_json::json!({"dsn": ":memory:v2:"}));
        Ok(())
    }
}

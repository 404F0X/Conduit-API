//! Project repository — Rust port of `conduit/internal/server/biz/project.go`.
//!
//! Mirrors the Go `ProjectService` data-access surface against the `Project`
//! Ent schema (`internal/ent/schema/project.go`): name-keyed lookup, soft
//! delete via `deleted_at`, status transitions, profiles JSON, and paginated
//! listing.
//!
//! ## Storage model (RUST-P3-002 S13)
//! `ProjectRow` is now a hand-written typed struct carrying the real Go entity
//! columns (name, status, description, profiles, timestamps, deleted_at).
//!
//! ## Soft delete (mirrors UserRepo / Go `SoftDeleteMixin`)
//! Every default query filters `deleted_at IS NULL`. `*_with_deleted` skips the
//! filter for restore/admin flows. This mirrors Go's `(name, deleted_at)` unique
//! index — a soft-deleted project frees its name for re-registration.
//!
//! ## Pagination
//! `list_projects` orders by `created_at ASC` with a stable `id ASC` tie-breaker.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::ProjectRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when creating a project. Mirrors Go's
/// `ent.CreateProjectInput` minus edges (users/roles/api_keys).
#[derive(Debug, Clone)]
pub struct CreateProjectInput {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Caller-supplied timestamp (epoch millis or ISO-8601). Parsed into
    /// `DateTime<Utc>` on the row.
    pub created_at: String,
}

/// Patch applied by `update_project`. Only non-`None` fields are written.
/// `profiles` uses the outer `Option` to distinguish "no change" from "clear".
#[derive(Debug, Default, Clone)]
pub struct UpdateProjectInput {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub status: Option<String>,
    pub profiles: Option<Value>,
    /// New `updated_at` value; required by the soft-delete mixin contract.
    pub updated_at: String,
}

/// Pagination/filter params for `list_projects`. `after_*` enable keyset-style
/// pagination on top of the limit/offset window.
#[derive(Debug, Clone)]
pub struct ListProjectsQuery {
    pub limit: u32,
    pub offset: u32,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl Default for ListProjectsQuery {
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
pub struct ListProjectsResult {
    pub rows: Vec<ProjectRow>,
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

fn is_live(row: &ProjectRow) -> bool {
    row.deleted_at.is_none()
}

fn row_from_input(input: &CreateProjectInput) -> ProjectRow {
    let now = parse_dt(&input.created_at);
    ProjectRow {
        id: input.id.clone(),
        name: input.name.clone(),
        status: "active".into(),
        description: input.description.clone().unwrap_or_default(),
        profiles: Value::Object(Default::default()),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut ProjectRow, input: &UpdateProjectInput) {
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(desc) = &input.description {
        row.description = desc.clone().unwrap_or_default();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    if let Some(profiles) = &input.profiles {
        row.profiles = profiles.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait ProjectRepo: Send + Sync {
    async fn create_project_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateProjectInput,
    ) -> RepoResult<ProjectRow>;

    async fn create_project(
        &self,
        ctx: &RequestContext,
        input: CreateProjectInput,
    ) -> RepoResult<ProjectRow> {
        guard_repo_principal(ctx)?;
        self.create_project_unchecked(ctx, input).await
    }

    async fn find_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>>;

    async fn find_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        guard_repo_principal(ctx)?;
        self.find_project_unchecked(ctx, project_id).await
    }

    async fn find_project_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>>;

    async fn find_project_with_deleted(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        guard_repo_principal(ctx)?;
        self.find_project_with_deleted_unchecked(ctx, project_id)
            .await
    }

    async fn find_project_by_name_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ProjectRow>>;

    async fn find_project_by_name(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        guard_repo_principal(ctx)?;
        self.find_project_by_name_unchecked(ctx, name).await
    }

    async fn list_projects_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListProjectsQuery,
    ) -> RepoResult<ListProjectsResult>;

    async fn list_projects(
        &self,
        ctx: &RequestContext,
        query: &ListProjectsQuery,
    ) -> RepoResult<ListProjectsResult> {
        guard_repo_principal(ctx)?;
        self.list_projects_unchecked(ctx, query).await
    }

    async fn update_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        input: UpdateProjectInput,
    ) -> RepoResult<ProjectRow>;

    async fn update_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        input: UpdateProjectInput,
    ) -> RepoResult<ProjectRow> {
        guard_repo_principal(ctx)?;
        self.update_project_unchecked(ctx, project_id, input).await
    }

    async fn soft_delete_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ProjectRow>;

    async fn soft_delete_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ProjectRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_project_unchecked(ctx, project_id, deleted_at)
            .await
    }

    async fn restore_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<ProjectRow>;

    async fn restore_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<ProjectRow> {
        guard_repo_principal(ctx)?;
        self.restore_project_unchecked(ctx, project_id).await
    }

    async fn project_exists_unchecked(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool>;

    async fn project_exists(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.project_exists_unchecked(ctx, name).await
    }

    async fn project_exists_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool>;

    async fn project_exists_with_deleted(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.project_exists_with_deleted_unchecked(ctx, name).await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryProjectRepo {
    rows: Mutex<BTreeMap<String, ProjectRow>>,
}

impl InMemoryProjectRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = ProjectRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    fn name_in_use_locked(
        guard: &BTreeMap<String, ProjectRow>,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| is_live(row) && row.name == name && Some(row.id.as_str()) != exclude_id)
    }
}

#[async_trait]
impl ProjectRepo for InMemoryProjectRepo {
    async fn create_project_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateProjectInput,
    ) -> RepoResult<ProjectRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?;
        if Self::name_in_use_locked(&guard, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("project id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .get(project_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_project_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .get(project_id)
            .cloned())
    }

    async fn find_project_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .values()
            .find(|r| is_live(r) && r.name == name)
            .cloned())
    }

    async fn list_projects_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListProjectsQuery,
    ) -> RepoResult<ListProjectsResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?;

        let mut live: Vec<ProjectRow> = guard.values().filter(|r| is_live(r)).cloned().collect();
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

        Ok(ListProjectsResult { rows, has_more })
    }

    async fn update_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        input: UpdateProjectInput,
    ) -> RepoResult<ProjectRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?;

        if let Some(new_name) = &input.name
            && Self::name_in_use_locked(&guard, new_name, Some(project_id))
        {
            return Err(RepoError::NameConflict);
        }

        let row = guard
            .get_mut(project_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("project"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ProjectRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?;
        let row = guard
            .get_mut(project_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("project"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        row.status = "archived".into();
        Ok(row.clone())
    }

    async fn restore_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<ProjectRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?;

        let (name, is_deleted) = guard
            .get(project_id)
            .map(|r| (r.name.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("project"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, &name, Some(project_id)) {
                return Err(RepoError::NameConflict);
            }
            let row = guard
                .get_mut(project_id)
                .ok_or(RepoError::NotFound("project"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "active".into();
        }
        Ok(guard
            .get(project_id)
            .ok_or(RepoError::NotFound("project"))?
            .clone())
    }

    async fn project_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .values()
            .any(|r| is_live(r) && r.name == name))
    }

    async fn project_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("project repo"))?
            .values()
            .any(|r| r.name == name))
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

    fn input(id: &str, name: &str, created_at: &str) -> CreateProjectInput {
        CreateProjectInput {
            id: id.into(),
            name: name.into(),
            description: Some("demo".into()),
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id_and_name() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_project(&ctx, input("p-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "p-1");
        assert_eq!(created.name, "Alpha");
        assert_eq!(created.status, "active");

        let by_id = repo
            .find_project(&ctx, "p-1")
            .await?
            .ok_or(RepoError::NotFound("p-1"))?;
        assert_eq!(by_id.id, "p-1");

        let by_name = repo
            .find_project_by_name(&ctx, "Alpha")
            .await?
            .ok_or(RepoError::NotFound("name"))?;
        assert_eq!(by_name.id, "p-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_project(&anon, "p-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_project(&anon, input("p-2", "Beta", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn name_conflict_on_create_and_update() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();
        repo.create_project(&ctx, input("p-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_project(&ctx, input("p-2", "Beta", "2024-01-02T00:00:00Z"))
            .await?;

        let dup = repo
            .create_project(&ctx, input("p-3", "Alpha", "2024-01-03T00:00:00Z"))
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));

        let rename = repo
            .update_project(
                &ctx,
                "p-2",
                UpdateProjectInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-04T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::NameConflict)));

        let self_rename = repo
            .update_project(
                &ctx,
                "p-1",
                UpdateProjectInput {
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
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();
        repo.create_project(&ctx, input("p-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let deleted = repo
            .soft_delete_project(&ctx, "p-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "archived");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_project(&ctx, "p-1").await?.is_none());
        assert!(repo.find_project_by_name(&ctx, "Alpha").await?.is_none());
        assert!(!repo.project_exists(&ctx, "Alpha").await?);
        assert!(repo.find_project_with_deleted(&ctx, "p-1").await?.is_some());
        assert!(repo.project_exists_with_deleted(&ctx, "Alpha").await?);
        Ok(())
    }

    #[tokio::test]
    async fn restore_brings_row_back_and_frees_name() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();
        repo.create_project(&ctx, input("p-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.soft_delete_project(&ctx, "p-1", "2024-02-01T00:00:00Z")
            .await?;

        repo.create_project(&ctx, input("p-2", "Alpha", "2024-03-01T00:00:00Z"))
            .await?;

        let restore_conflict = repo.restore_project(&ctx, "p-1").await;
        assert!(matches!(restore_conflict, Err(RepoError::NameConflict)));

        repo.soft_delete_project(&ctx, "p-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_project(&ctx, "p-1").await?;
        assert_eq!(restored.status, "active");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_project(&ctx, "p-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_project(&ctx, input("c", "C", ts)).await?;
        repo.create_project(&ctx, input("a", "A", ts)).await?;
        repo.create_project(&ctx, input("b", "B", ts)).await?;

        let page1 = repo
            .list_projects(
                &ctx,
                &ListProjectsQuery {
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
            .list_projects(
                &ctx,
                &ListProjectsQuery {
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
    async fn update_profiles_and_status() -> RepoResult<()> {
        let repo = InMemoryProjectRepo::new();
        let ctx = ctx_allowed();
        repo.create_project(&ctx, input("p-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let updated = repo
            .update_project(
                &ctx,
                "p-1",
                UpdateProjectInput {
                    status: Some("archived".into()),
                    profiles: Some(serde_json::json!({"active": "default"})),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, "archived");
        assert_eq!(updated.profiles, serde_json::json!({"active": "default"}));
        Ok(())
    }
}

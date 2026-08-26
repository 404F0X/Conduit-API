//! Role repository — Rust port of `conduit/internal/server/biz/role.go`.
//!
//! RUST-P3-002 S13: `RoleRow` is now a hand-written typed struct.
//!
//! ## System vs project roles
//! Go stores `project_id = NULL` for system roles. The Rust `RoleRow` uses
//! `project_id == ""` (empty string) as the system-scope sentinel.
//!
//! ## Uniqueness (mirrors Go `Role.Indexes`)
//! `roles_by_project_id_name` on `(project_id, name)` — NO `deleted_at`. A
//! soft-deleted row does NOT free its name (real DB constraint).
//!
//! ## Soft delete
//! Go's `SoftDeleteMixin` sets ONLY `deleted_at`; status is not a Go column.
//! The InMemory impl tracks "active"/"deactivated" on `status` for convenience.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::RoleRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Role level enum — mirrors Go's `role.Level`.
pub const LEVEL_SYSTEM: &str = "system";
pub const LEVEL_PROJECT: &str = "project";

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

fn is_live(row: &RoleRow) -> bool {
    row.deleted_at.is_none()
}

fn is_system_role(row: &RoleRow) -> bool {
    row.project_id.is_empty()
}

/// Fields a caller may set when creating a role.
#[derive(Debug, Clone)]
pub struct CreateRoleInput {
    pub id: String,
    pub name: String,
    pub level: String,
    pub project_id: String,
    pub scopes: Vec<String>,
    pub created_at: String,
}

/// Patch applied by `update_role`.
#[derive(Debug, Default, Clone)]
pub struct UpdateRoleInput {
    pub name: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ListRolesQuery {
    pub limit: u32,
    pub offset: u32,
    pub project_id: Option<String>,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl Default for ListRolesQuery {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            project_id: None,
            after_created_at: None,
            after_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListRolesResult {
    pub rows: Vec<RoleRow>,
    pub has_more: bool,
}

fn row_from_input(input: &CreateRoleInput) -> RoleRow {
    let now = parse_dt(&input.created_at);
    RoleRow {
        id: input.id.clone(),
        name: input.name.clone(),
        level: input.level.clone(),
        project_id: input.project_id.clone(),
        scopes: input.scopes.clone(),
        status: "active".into(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut RoleRow, input: &UpdateRoleInput) {
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(scopes) = &input.scopes {
        row.scopes = scopes.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait RoleRepo: Send + Sync {
    async fn create_role_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateRoleInput,
    ) -> RepoResult<RoleRow>;

    async fn create_role(
        &self,
        ctx: &RequestContext,
        input: CreateRoleInput,
    ) -> RepoResult<RoleRow> {
        guard_repo_principal(ctx)?;
        self.create_role_unchecked(ctx, input).await
    }

    async fn find_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>>;

    async fn find_role(&self, ctx: &RequestContext, role_id: &str) -> RepoResult<Option<RoleRow>> {
        guard_repo_principal(ctx)?;
        self.find_role_unchecked(ctx, role_id).await
    }

    async fn find_role_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>>;

    async fn find_role_with_deleted(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>> {
        guard_repo_principal(ctx)?;
        self.find_role_with_deleted_unchecked(ctx, role_id).await
    }

    async fn list_system_roles_unchecked(&self, ctx: &RequestContext) -> RepoResult<Vec<RoleRow>>;

    async fn list_system_roles(&self, ctx: &RequestContext) -> RepoResult<Vec<RoleRow>> {
        guard_repo_principal(ctx)?;
        self.list_system_roles_unchecked(ctx).await
    }

    async fn list_roles_by_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<RoleRow>>;

    async fn list_roles_by_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<RoleRow>> {
        guard_repo_principal(ctx)?;
        self.list_roles_by_project_unchecked(ctx, project_id).await
    }

    async fn list_roles_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListRolesQuery,
    ) -> RepoResult<ListRolesResult>;

    async fn list_roles(
        &self,
        ctx: &RequestContext,
        query: &ListRolesQuery,
    ) -> RepoResult<ListRolesResult> {
        guard_repo_principal(ctx)?;
        self.list_roles_unchecked(ctx, query).await
    }

    async fn update_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        input: UpdateRoleInput,
    ) -> RepoResult<RoleRow>;

    async fn update_role(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        input: UpdateRoleInput,
    ) -> RepoResult<RoleRow> {
        guard_repo_principal(ctx)?;
        self.update_role_unchecked(ctx, role_id, input).await
    }

    async fn soft_delete_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        deleted_at: &str,
    ) -> RepoResult<RoleRow>;

    async fn soft_delete_role(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        deleted_at: &str,
    ) -> RepoResult<RoleRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_role_unchecked(ctx, role_id, deleted_at)
            .await
    }

    async fn restore_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<RoleRow>;

    async fn restore_role(&self, ctx: &RequestContext, role_id: &str) -> RepoResult<RoleRow> {
        guard_repo_principal(ctx)?;
        self.restore_role_unchecked(ctx, role_id).await
    }

    async fn role_name_exists_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool>;

    async fn role_name_exists(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.role_name_exists_unchecked(ctx, project_id, name).await
    }

    async fn role_name_exists_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool>;

    async fn role_name_exists_with_deleted(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.role_name_exists_with_deleted_unchecked(ctx, project_id, name)
            .await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryRoleRepo {
    rows: Mutex<BTreeMap<String, RoleRow>>,
}

impl InMemoryRoleRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = RoleRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    fn name_in_use_locked(
        guard: &BTreeMap<String, RoleRow>,
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
impl RoleRepo for InMemoryRoleRepo {
    async fn create_role_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateRoleInput,
    ) -> RepoResult<RoleRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;
        if Self::name_in_use_locked(&guard, &input.project_id, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("role id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_role_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?
            .get(role_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_role_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?
            .get(role_id)
            .cloned())
    }

    async fn list_system_roles_unchecked(&self, _ctx: &RequestContext) -> RepoResult<Vec<RoleRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;
        let mut rows: Vec<RoleRow> = guard
            .values()
            .filter(|r| is_live(r) && is_system_role(r))
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    async fn list_roles_by_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<RoleRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;
        let mut rows: Vec<RoleRow> = guard
            .values()
            .filter(|r| is_live(r) && !is_system_role(r) && r.project_id == project_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    async fn list_roles_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListRolesQuery,
    ) -> RepoResult<ListRolesResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;

        let mut live: Vec<RoleRow> = guard
            .values()
            .filter(|r| {
                is_live(r)
                    && match &query.project_id {
                        Some(pid) => r.project_id == *pid,
                        None => true,
                    }
            })
            .cloned()
            .collect();

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

        Ok(ListRolesResult { rows, has_more })
    }

    async fn update_role_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
        input: UpdateRoleInput,
    ) -> RepoResult<RoleRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;

        if let Some(new_name) = &input.name {
            let project_id = guard
                .get(role_id)
                .map(|r| r.project_id.clone())
                .ok_or(RepoError::NotFound("role"))?;
            if Self::name_in_use_locked(&guard, &project_id, new_name, Some(role_id)) {
                return Err(RepoError::NameConflict);
            }
        }

        let row = guard
            .get_mut(role_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("role"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_role_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
        deleted_at: &str,
    ) -> RepoResult<RoleRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;
        let row = guard
            .get_mut(role_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("role"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        row.status = "deactivated".into();
        Ok(row.clone())
    }

    async fn restore_role_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<RoleRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;

        let (project_id, name, is_deleted) = guard
            .get(role_id)
            .map(|r| (r.project_id.clone(), r.name.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("role"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, &project_id, &name, Some(role_id)) {
                return Err(RepoError::NameConflict);
            }
            let row = guard.get_mut(role_id).ok_or(RepoError::NotFound("role"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "active".into();
        }
        Ok(guard
            .get(role_id)
            .ok_or(RepoError::NotFound("role"))?
            .clone())
    }

    async fn role_name_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?;
        Ok(Self::name_in_use_locked(&guard, project_id, name, None))
    }

    async fn role_name_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("role repo"))?
            .values()
            .any(|r| r.project_id == project_id && r.name == name))
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

    fn system_input(id: &str, name: &str, created_at: &str) -> CreateRoleInput {
        CreateRoleInput {
            id: id.into(),
            name: name.into(),
            level: LEVEL_SYSTEM.into(),
            project_id: String::new(),
            scopes: vec!["read".into()],
            created_at: created_at.into(),
        }
    }

    fn project_input(id: &str, name: &str, project_id: &str, created_at: &str) -> CreateRoleInput {
        CreateRoleInput {
            id: id.into(),
            name: name.into(),
            level: LEVEL_PROJECT.into(),
            project_id: project_id.into(),
            scopes: vec!["read".into()],
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_role(&ctx, system_input("r-1", "Admin", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "r-1");
        assert_eq!(created.name, "Admin");
        assert_eq!(created.level, LEVEL_SYSTEM);
        assert!(is_system_role(&created));

        let by_id = repo
            .find_role(&ctx, "r-1")
            .await?
            .ok_or(RepoError::NotFound("r-1"))?;
        assert_eq!(by_id.id, "r-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_role(&anon, "r-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_role(&anon, system_input("r-2", "Admin", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn name_unique_within_project_scope() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();

        repo.create_role(
            &ctx,
            project_input("r-1", "Admin", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.create_role(
            &ctx,
            project_input("r-2", "Admin", "p-2", "2024-01-02T00:00:00Z"),
        )
        .await?;

        let dup = repo
            .create_role(
                &ctx,
                project_input("r-3", "Admin", "p-1", "2024-01-03T00:00:00Z"),
            )
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));

        repo.create_role(&ctx, system_input("r-4", "Admin", "2024-01-04T00:00:00Z"))
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn name_conflict_on_update() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();
        repo.create_role(
            &ctx,
            project_input("r-1", "Admin", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.create_role(
            &ctx,
            project_input("r-2", "Viewer", "p-1", "2024-01-02T00:00:00Z"),
        )
        .await?;

        let rename = repo
            .update_role(
                &ctx,
                "r-2",
                UpdateRoleInput {
                    name: Some("Admin".into()),
                    updated_at: "2024-01-03T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::NameConflict)));

        let self_rename = repo
            .update_role(
                &ctx,
                "r-1",
                UpdateRoleInput {
                    name: Some("Admin".into()),
                    updated_at: "2024-01-04T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(self_rename.name, "Admin");
        Ok(())
    }

    #[tokio::test]
    async fn system_role_query_excludes_project_roles() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();
        repo.create_role(
            &ctx,
            system_input("r-1", "SuperAdmin", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.create_role(&ctx, system_input("r-2", "Auditor", "2024-01-02T00:00:00Z"))
            .await?;
        repo.create_role(
            &ctx,
            project_input("r-3", "Admin", "p-1", "2024-01-03T00:00:00Z"),
        )
        .await?;
        repo.create_role(
            &ctx,
            project_input("r-4", "Viewer", "p-1", "2024-01-04T00:00:00Z"),
        )
        .await?;

        let sys = repo.list_system_roles(&ctx).await?;
        let names: Vec<_> = sys.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["SuperAdmin", "Auditor"]);
        assert!(sys.iter().all(is_system_role));

        let proj = repo.list_roles_by_project(&ctx, "p-1").await?;
        let proj_names: Vec<_> = proj.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(proj_names, vec!["Admin", "Viewer"]);
        assert!(proj.iter().all(|r| !is_system_role(r)));
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_then_restore() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();
        repo.create_role(
            &ctx,
            project_input("r-1", "Admin", "p-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        let deleted = repo
            .soft_delete_role(&ctx, "r-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "deactivated");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_role(&ctx, "r-1").await?.is_none());
        assert!(!repo.role_name_exists(&ctx, "p-1", "Admin").await?);
        assert!(repo.find_role_with_deleted(&ctx, "r-1").await?.is_some());

        repo.create_role(
            &ctx,
            project_input("r-2", "Admin", "p-1", "2024-03-01T00:00:00Z"),
        )
        .await?;

        let conflict = repo.restore_role(&ctx, "r-1").await;
        assert!(matches!(conflict, Err(RepoError::NameConflict)));

        repo.soft_delete_role(&ctx, "r-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_role(&ctx, "r-1").await?;
        assert_eq!(restored.status, "active");
        assert!(restored.deleted_at.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_role(&ctx, project_input("c", "C", "p-1", ts))
            .await?;
        repo.create_role(&ctx, project_input("a", "A", "p-1", ts))
            .await?;
        repo.create_role(&ctx, project_input("b", "B", "p-1", ts))
            .await?;

        let page1 = repo
            .list_roles(
                &ctx,
                &ListRolesQuery {
                    project_id: Some("p-1".into()),
                    limit: 2,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert!(page1.has_more);

        let page2 = repo
            .list_roles(
                &ctx,
                &ListRolesQuery {
                    project_id: Some("p-1".into()),
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
    async fn role_name_exists_matches_go_semantics() -> RepoResult<()> {
        let repo = InMemoryRoleRepo::new();
        let ctx = ctx_allowed();
        repo.create_role(
            &ctx,
            system_input("r-1", "SuperAdmin", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.create_role(
            &ctx,
            project_input("r-2", "Admin", "p-1", "2024-01-02T00:00:00Z"),
        )
        .await?;

        assert!(repo.role_name_exists(&ctx, "", "SuperAdmin").await?);
        assert!(!repo.role_name_exists(&ctx, "", "Admin").await?);
        assert!(repo.role_name_exists(&ctx, "p-1", "Admin").await?);
        assert!(!repo.role_name_exists(&ctx, "p-2", "Admin").await?);
        Ok(())
    }
}

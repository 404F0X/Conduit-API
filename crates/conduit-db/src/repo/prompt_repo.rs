//! Prompt repository — Rust port of `conduit/internal/server/biz/prompt.go`.
//!
//! RUST-P3-002 S13: `PromptRow` is now a hand-written typed struct.
//!
//! ## Uniqueness (mirrors Go `Prompt.Indexes`)
//! `prompts_by_project_id_name` is a composite unique index on
//! `(project_id, name, deleted_at)`. A soft-deleted row frees its name for
//! re-registration within the same project.
//!
//! ## `order` field
//! `order` is a SQL reserved keyword; the Rust field is `order_val` with
//! `#[sqlx(rename = "order")]`.
//!
//! ## Status semantics
//! Go enum: `enabled | disabled`; default `disabled`. Soft delete does NOT
//! change status (Go's `SoftDeleteMixin` only sets `deleted_at`).

use crate::repo::{RepoError, RepoResult, RequestContext, guard_project_access};
use crate::row::PromptRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use crate::policy::ProjectAccess;

/// Status values — mirror Go's `prompt.Status` enum.
pub const STATUS_ENABLED: &str = "enabled";
pub const STATUS_DISABLED: &str = "disabled";

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

fn is_live(row: &PromptRow) -> bool {
    row.deleted_at.is_none()
}

/// Fields a caller may set when creating a prompt.
#[derive(Debug, Clone)]
pub struct CreatePromptInput {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub description: Option<String>,
    pub role: String,
    pub content: String,
    pub status: Option<String>,
    pub order: Option<i64>,
    pub settings: Option<Value>,
    pub created_at: String,
}

/// Patch applied by `update_prompt`. Only non-`None` fields are written.
#[derive(Debug, Default, Clone)]
pub struct UpdatePromptInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub content: Option<String>,
    pub order: Option<i64>,
    pub status: Option<String>,
    pub settings: Option<Value>,
    pub updated_at: String,
}

fn row_from_input(input: &CreatePromptInput) -> PromptRow {
    let now = parse_dt(&input.created_at);
    PromptRow {
        id: input.id.clone(),
        project_id: input.project_id.clone(),
        name: input.name.clone(),
        status: input
            .status
            .clone()
            .unwrap_or_else(|| STATUS_DISABLED.into()),
        description: input.description.clone().unwrap_or_default(),
        role: input.role.clone(),
        content: input.content.clone(),
        order_val: input.order.unwrap_or(0),
        settings: input
            .settings
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default())),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut PromptRow, input: &UpdatePromptInput) {
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(desc) = &input.description {
        row.description = desc.clone();
    }
    if let Some(role) = &input.role {
        row.role = role.clone();
    }
    if let Some(content) = &input.content {
        row.content = content.clone();
    }
    if let Some(order) = input.order {
        row.order_val = order;
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    if let Some(settings) = &input.settings {
        row.settings = settings.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait PromptRepo: Send + Sync {
    async fn create_prompt_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreatePromptInput,
    ) -> RepoResult<PromptRow>;

    async fn create_prompt(
        &self,
        ctx: &RequestContext,
        input: CreatePromptInput,
    ) -> RepoResult<PromptRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        self.create_prompt_unchecked(ctx, input).await
    }

    async fn find_prompt_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>>;

    async fn find_prompt(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_prompt_unchecked(ctx, project_id, prompt_id).await
    }

    async fn find_prompt_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>>;

    async fn find_prompt_with_deleted(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_prompt_with_deleted_unchecked(ctx, project_id, prompt_id)
            .await
    }

    async fn list_prompts_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<PromptRow>>;

    /// Enumerate projects that currently own at least one live prompt.
    /// GraphQL's admin connection is cross-project while the row-list method
    /// is intentionally project-scoped, so adapters use this repository-owned
    /// projection instead of reaching through a concrete backend to raw SQL.
    async fn list_live_prompt_project_ids_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<String>>;

    async fn list_prompts(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<PromptRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.list_prompts_unchecked(ctx, project_id).await
    }

    async fn update_prompt_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        input: UpdatePromptInput,
    ) -> RepoResult<PromptRow>;

    async fn update_prompt(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        input: UpdatePromptInput,
    ) -> RepoResult<PromptRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.update_prompt_unchecked(ctx, project_id, prompt_id, input)
            .await
    }

    async fn set_prompt_status_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        status: String,
        updated_at: String,
    ) -> RepoResult<PromptRow>;

    async fn set_prompt_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        status: String,
        updated_at: String,
    ) -> RepoResult<PromptRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.set_prompt_status_unchecked(ctx, project_id, prompt_id, status, updated_at)
            .await
    }

    async fn soft_delete_prompt_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        deleted_at: String,
    ) -> RepoResult<PromptRow>;

    async fn soft_delete_prompt(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        deleted_at: String,
    ) -> RepoResult<PromptRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.soft_delete_prompt_unchecked(ctx, project_id, prompt_id, deleted_at)
            .await
    }

    async fn restore_prompt_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<PromptRow>;

    async fn restore_prompt(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<PromptRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.restore_prompt_unchecked(ctx, project_id, prompt_id)
            .await
    }
}

// --- in-memory impl --------------------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryPromptRepo {
    rows: Mutex<BTreeMap<String, PromptRow>>,
}

impl InMemoryPromptRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    fn name_in_use_locked(
        guard: &BTreeMap<String, PromptRow>,
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
impl PromptRepo for InMemoryPromptRepo {
    async fn create_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreatePromptInput,
    ) -> RepoResult<PromptRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?;

        if Self::name_in_use_locked(&guard, &input.project_id, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("prompt id already present"));
        }

        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?
            .get(prompt_id)
            .filter(|r| r.project_id == project_id && is_live(r))
            .cloned())
    }

    async fn find_prompt_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?
            .get(prompt_id)
            .filter(|r| r.project_id == project_id)
            .cloned())
    }

    async fn list_prompts_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<PromptRow>> {
        // Order by `order_val` ASC, then id ASC.
        let mut rows: Vec<PromptRow> = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?
            .values()
            .filter(|r| r.project_id == project_id && is_live(r))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.order_val.cmp(&b.order_val).then_with(|| a.id.cmp(&b.id)));
        Ok(rows)
    }

    async fn list_live_prompt_project_ids_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<String>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?
            .values()
            .filter(|row| is_live(row))
            .map(|row| row.project_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    async fn update_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        input: UpdatePromptInput,
    ) -> RepoResult<PromptRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?;

        if let Some(new_name) = &input.name
            && Self::name_in_use_locked(&guard, project_id, new_name, Some(prompt_id))
        {
            return Err(RepoError::NameConflict);
        }

        let row = guard
            .get_mut(prompt_id)
            .filter(|r| r.project_id == project_id && is_live(r))
            .ok_or(RepoError::NotFound("prompt"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn set_prompt_status_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        status: String,
        updated_at: String,
    ) -> RepoResult<PromptRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?;
        let row = guard
            .get_mut(prompt_id)
            .filter(|r| r.project_id == project_id && is_live(r))
            .ok_or(RepoError::NotFound("prompt"))?;
        row.status = status;
        row.updated_at = parse_dt(&updated_at);
        Ok(row.clone())
    }

    async fn soft_delete_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        deleted_at_ts: String,
    ) -> RepoResult<PromptRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?;
        let row = guard
            .get_mut(prompt_id)
            .filter(|r| r.project_id == project_id && is_live(r))
            .ok_or(RepoError::NotFound("prompt"))?;
        let ts = parse_dt(&deleted_at_ts);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        Ok(row.clone())
    }

    async fn restore_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<PromptRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("prompt repo"))?;

        let (name, is_deleted) = guard
            .get(prompt_id)
            .filter(|r| r.project_id == project_id)
            .map(|r| (r.name.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("prompt"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, project_id, &name, Some(prompt_id)) {
                return Err(RepoError::NameConflict);
            }
            let row = guard
                .get_mut(prompt_id)
                .ok_or(RepoError::NotFound("prompt"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
        }
        Ok(guard
            .get(prompt_id)
            .ok_or(RepoError::NotFound("prompt"))?
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use serde_json::json;

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn input(id: &str, project: &str, name: &str) -> CreatePromptInput {
        CreatePromptInput {
            id: id.into(),
            project_id: project.into(),
            name: name.into(),
            description: Some("desc".into()),
            role: "system".into(),
            content: "be helpful".into(),
            status: None,
            order: Some(1),
            settings: Some(json!({"conditions": []})),
            created_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_round_trip() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        assert_eq!(created.name, "greet");
        assert_eq!(created.status, STATUS_DISABLED);
        assert_eq!(created.order_val, 1);
        assert_eq!(created.settings, json!({"conditions": []}));

        let found = repo.find_prompt(&ctx, "proj-1", "p-1").await?;
        assert_eq!(found.map(|r| r.id), Some("p-1".into()));
        Ok(())
    }

    #[tokio::test]
    async fn name_conflict_within_project() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        let err = repo
            .create_prompt(&ctx, input("p-2", "proj-1", "greet"))
            .await;
        assert!(matches!(err, Err(RepoError::NameConflict)));
        Ok(())
    }

    #[tokio::test]
    async fn same_name_in_different_projects_is_ok() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        let a = repo
            .create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        let b = repo
            .create_prompt(&ctx, input("p-2", "proj-2", "greet"))
            .await?;
        assert_ne!(a.id, b.id);
        assert_eq!(repo.len()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_then_recreate_same_name() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        repo.soft_delete_prompt(&ctx, "proj-1", "p-1", "2024-02-01T00:00:00Z".into())
            .await?;

        let recreated = repo
            .create_prompt(&ctx, input("p-2", "proj-1", "greet"))
            .await?;
        assert_eq!(recreated.id, "p-2");

        let live = repo.find_prompt(&ctx, "proj-1", "p-1").await?;
        assert!(live.is_none());
        let with_del = repo.find_prompt_with_deleted(&ctx, "proj-1", "p-1").await?;
        assert!(with_del.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn restore_rechecks_name_conflict() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        repo.soft_delete_prompt(&ctx, "proj-1", "p-1", "2024-02-01T00:00:00Z".into())
            .await?;
        repo.create_prompt(&ctx, input("p-2", "proj-1", "greet"))
            .await?;

        let err = repo.restore_prompt(&ctx, "proj-1", "p-1").await;
        assert!(matches!(err, Err(RepoError::NameConflict)));
        Ok(())
    }

    #[tokio::test]
    async fn restore_live_row_is_noop() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "greet"))
            .await?;
        let restored = repo.restore_prompt(&ctx, "proj-1", "p-1").await?;
        assert_eq!(restored.id, "p-1");
        assert!(is_live(&restored));
        Ok(())
    }

    #[tokio::test]
    async fn list_by_project_orders_by_order_then_id() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        let mut i1 = input("p-1", "proj-1", "a");
        i1.order = Some(2);
        let mut i2 = input("p-2", "proj-1", "b");
        i2.order = Some(1);
        let mut i3 = input("p-3", "proj-1", "c");
        i3.order = Some(1);
        repo.create_prompt(&ctx, i1).await?;
        repo.create_prompt(&ctx, i2).await?;
        repo.create_prompt(&ctx, i3).await?;
        repo.create_prompt(&ctx, input("p-9", "proj-2", "x"))
            .await?;

        let rows = repo.list_prompts(&ctx, "proj-1").await?;
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["p-2", "p-3", "p-1"]);
        Ok(())
    }

    #[tokio::test]
    async fn list_excludes_soft_deleted() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "a"))
            .await?;
        repo.create_prompt(&ctx, input("p-2", "proj-1", "b"))
            .await?;
        repo.soft_delete_prompt(&ctx, "proj-1", "p-2", "2024-02-01T00:00:00Z".into())
            .await?;

        let rows = repo.list_prompts(&ctx, "proj-1").await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "p-1");
        Ok(())
    }

    #[tokio::test]
    async fn update_changes_fields_and_rechecks_name() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "a"))
            .await?;
        repo.create_prompt(&ctx, input("p-2", "proj-1", "b"))
            .await?;

        let err = repo
            .update_prompt(
                &ctx,
                "proj-1",
                "p-1",
                UpdatePromptInput {
                    name: Some("b".into()),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(err, Err(RepoError::NameConflict)));

        let updated = repo
            .update_prompt(
                &ctx,
                "proj-1",
                "p-1",
                UpdatePromptInput {
                    name: Some("c".into()),
                    content: Some("new content".into()),
                    updated_at: "2024-03-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.name, "c");
        assert_eq!(updated.content, "new content");
        Ok(())
    }

    #[tokio::test]
    async fn set_status_transitions() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "a"))
            .await?;
        let enabled = repo
            .set_prompt_status(
                &ctx,
                "proj-1",
                "p-1",
                STATUS_ENABLED.into(),
                "2024-02-01T00:00:00Z".into(),
            )
            .await?;
        assert_eq!(enabled.status, STATUS_ENABLED);
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_unknown_returns_not_found() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        let err = repo
            .soft_delete_prompt(&ctx, "proj-1", "missing", "2024-02-01T00:00:00Z".into())
            .await;
        assert!(matches!(err, Err(RepoError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_cross_project_returns_not_found() -> RepoResult<()> {
        let repo = InMemoryPromptRepo::new();
        let ctx = ctx_allowed();

        repo.create_prompt(&ctx, input("p-1", "proj-1", "a"))
            .await?;
        let err = repo
            .soft_delete_prompt(&ctx, "proj-2", "p-1", "2024-02-01T00:00:00Z".into())
            .await;
        assert!(matches!(err, Err(RepoError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_denies_anonymous() {
        let repo = InMemoryPromptRepo::new();
        let anon = ctx_anon();

        let denied = repo.create_prompt(&anon, input("p-1", "proj-1", "a")).await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));
        assert_eq!(repo.len(), Ok(0));
    }
}

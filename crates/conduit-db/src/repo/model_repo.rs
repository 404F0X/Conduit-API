//! Model repository — Rust port of `conduit/internal/server/biz/model.go`.
//!
//! Mirrors the Go `ModelService` data-access surface against the `Model` Ent
//! schema (`internal/ent/schema/model.go`): lookups by name and by `model_id`,
//! soft delete via `deleted_at`, status transitions, paginated listing, and
//! `model_card`/`settings` JSON.
//!
//! ## Storage model (RUST-P3-002 S13)
//! `ModelRow` is now a hand-written typed struct carrying the real Go entity
//! columns (developer, model_id, name, type, icon, group, model_card, settings,
//! remark, timestamps, deleted_at).
//!
//! ## Uniqueness (mirrors Go `Model.Indexes`)
//! - `models_by_name`     -> `(name, deleted_at)`
//! - `models_by_model_id` -> `(model_id, deleted_at)`
//!
//! ## Status semantics
//! Go enum: `enabled | disabled | archived`; default on create is `disabled`.
//! Soft delete sets `archived`.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::ModelRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when creating a model.
#[derive(Debug, Clone)]
pub struct CreateModelInput {
    pub id: String,
    pub developer: String,
    pub model_id: String,
    pub name: String,
    pub model_type: Option<String>,
    pub icon: Option<String>,
    pub group: String,
    pub model_card: Option<Value>,
    pub settings: Option<Value>,
    pub remark: Option<String>,
    pub created_at: String,
}

/// Patch applied by `update_model`. Only non-`None` fields are written.
#[derive(Debug, Default, Clone)]
pub struct UpdateModelInput {
    pub developer: Option<String>,
    pub model_id: Option<String>,
    pub name: Option<String>,
    pub model_type: Option<String>,
    pub icon: Option<Option<String>>,
    pub group: Option<String>,
    pub model_card: Option<Value>,
    pub settings: Option<Value>,
    pub remark: Option<Option<String>>,
    pub status: Option<String>,
    pub updated_at: String,
}

/// Pagination/filter params for `list_models`.
#[derive(Debug, Clone)]
pub struct ListModelsQuery {
    pub limit: u32,
    pub offset: u32,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl Default for ListModelsQuery {
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
pub struct ListModelsResult {
    pub rows: Vec<ModelRow>,
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

fn is_live(row: &ModelRow) -> bool {
    row.deleted_at.is_none()
}

fn row_from_input(input: &CreateModelInput) -> ModelRow {
    let now = parse_dt(&input.created_at);
    ModelRow {
        id: input.id.clone(),
        name: input.name.clone(),
        status: "disabled".into(),
        developer: input.developer.clone(),
        model_id: input.model_id.clone(),
        model_type: input.model_type.clone().unwrap_or_else(|| "chat".into()),
        icon: input.icon.clone().unwrap_or_default(),
        group_name: input.group.clone(),
        model_card: input.model_card.clone().unwrap_or_default(),
        settings: input.settings.clone().unwrap_or_default(),
        remark: input.remark.clone(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut ModelRow, input: &UpdateModelInput) {
    if let Some(developer) = &input.developer {
        row.developer = developer.clone();
    }
    if let Some(model_id) = &input.model_id {
        row.model_id = model_id.clone();
    }
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(model_type) = &input.model_type {
        row.model_type = model_type.clone();
    }
    if let Some(icon) = &input.icon {
        row.icon = icon.clone().unwrap_or_default();
    }
    if let Some(group) = &input.group {
        row.group_name = group.clone();
    }
    if let Some(model_card) = &input.model_card {
        row.model_card = model_card.clone();
    }
    if let Some(settings) = &input.settings {
        row.settings = settings.clone();
    }
    if let Some(remark) = &input.remark {
        row.remark = remark.clone();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait ModelRepo: Send + Sync {
    async fn create_model_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateModelInput,
    ) -> RepoResult<ModelRow>;

    async fn create_model(
        &self,
        ctx: &RequestContext,
        input: CreateModelInput,
    ) -> RepoResult<ModelRow> {
        guard_repo_principal(ctx)?;
        self.create_model_unchecked(ctx, input).await
    }

    async fn find_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>>;

    async fn find_model(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        guard_repo_principal(ctx)?;
        self.find_model_unchecked(ctx, model_id).await
    }

    async fn find_model_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>>;

    async fn find_model_with_deleted(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        guard_repo_principal(ctx)?;
        self.find_model_with_deleted_unchecked(ctx, model_id).await
    }

    async fn find_model_by_name_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ModelRow>>;

    async fn find_model_by_name(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ModelRow>> {
        guard_repo_principal(ctx)?;
        self.find_model_by_name_unchecked(ctx, name).await
    }

    async fn find_model_by_model_id_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>>;

    async fn find_model_by_model_id(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        guard_repo_principal(ctx)?;
        self.find_model_by_model_id_unchecked(ctx, model_id).await
    }

    async fn list_models_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListModelsQuery,
    ) -> RepoResult<ListModelsResult>;

    async fn list_models(
        &self,
        ctx: &RequestContext,
        query: &ListModelsQuery,
    ) -> RepoResult<ListModelsResult> {
        guard_repo_principal(ctx)?;
        self.list_models_unchecked(ctx, query).await
    }

    async fn update_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        input: UpdateModelInput,
    ) -> RepoResult<ModelRow>;

    async fn update_model(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        input: UpdateModelInput,
    ) -> RepoResult<ModelRow> {
        guard_repo_principal(ctx)?;
        self.update_model_unchecked(ctx, model_id, input).await
    }

    async fn soft_delete_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ModelRow>;

    async fn soft_delete_model(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ModelRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_model_unchecked(ctx, model_id, deleted_at)
            .await
    }

    async fn restore_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<ModelRow>;

    async fn restore_model(&self, ctx: &RequestContext, model_id: &str) -> RepoResult<ModelRow> {
        guard_repo_principal(ctx)?;
        self.restore_model_unchecked(ctx, model_id).await
    }

    async fn model_exists_unchecked(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool>;

    async fn model_exists(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.model_exists_unchecked(ctx, name).await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryModelRepo {
    rows: Mutex<BTreeMap<String, ModelRow>>,
}

impl InMemoryModelRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = ModelRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    fn name_in_use_locked(
        guard: &BTreeMap<String, ModelRow>,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| is_live(row) && row.name == name && Some(row.id.as_str()) != exclude_id)
    }

    fn model_id_in_use_locked(
        guard: &BTreeMap<String, ModelRow>,
        model_id: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard.values().any(|row| {
            is_live(row) && row.model_id == model_id && Some(row.id.as_str()) != exclude_id
        })
    }
}

#[async_trait]
impl ModelRepo for InMemoryModelRepo {
    async fn create_model_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateModelInput,
    ) -> RepoResult<ModelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?;
        if Self::name_in_use_locked(&guard, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if Self::model_id_in_use_locked(&guard, &input.model_id, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("model id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_model_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
            .get(model_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_model_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
            .get(model_id)
            .cloned())
    }

    async fn find_model_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ModelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
            .values()
            .find(|r| is_live(r) && r.name == name)
            .cloned())
    }

    async fn find_model_by_model_id_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
            .values()
            .find(|r| is_live(r) && r.model_id == model_id)
            .cloned())
    }

    async fn list_models_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListModelsQuery,
    ) -> RepoResult<ListModelsResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?;

        let mut live: Vec<ModelRow> = guard.values().filter(|r| is_live(r)).cloned().collect();
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

        Ok(ListModelsResult { rows, has_more })
    }

    async fn update_model_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
        input: UpdateModelInput,
    ) -> RepoResult<ModelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?;

        if let Some(new_name) = &input.name
            && Self::name_in_use_locked(&guard, new_name, Some(model_id))
        {
            return Err(RepoError::NameConflict);
        }
        if let Some(new_model_id) = &input.model_id
            && Self::model_id_in_use_locked(&guard, new_model_id, Some(model_id))
        {
            return Err(RepoError::NameConflict);
        }

        let row = guard
            .get_mut(model_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("model"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_model_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ModelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?;
        let row = guard
            .get_mut(model_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("model"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        row.status = "archived".into();
        Ok(row.clone())
    }

    async fn restore_model_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<ModelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?;

        let (name, mid, is_deleted) = guard
            .get(model_id)
            .map(|r| (r.name.clone(), r.model_id.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("model"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, &name, Some(model_id))
                || Self::model_id_in_use_locked(&guard, &mid, Some(model_id))
            {
                return Err(RepoError::NameConflict);
            }
            let row = guard
                .get_mut(model_id)
                .ok_or(RepoError::NotFound("model"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "disabled".into();
        }
        Ok(guard
            .get(model_id)
            .ok_or(RepoError::NotFound("model"))?
            .clone())
    }

    async fn model_exists_unchecked(&self, _ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("model repo"))?
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

    fn input(id: &str, name: &str, model_id: &str, created_at: &str) -> CreateModelInput {
        CreateModelInput {
            id: id.into(),
            developer: "deepseek".into(),
            model_id: model_id.into(),
            name: name.into(),
            model_type: Some("chat".into()),
            icon: None,
            group: "deepseek".into(),
            model_card: Some(serde_json::json!({"summary": "demo"})),
            settings: Some(serde_json::json!({"upstream": "v1"})),
            remark: None,
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id_name_model_id() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_model(
                &ctx,
                input(
                    "m-1",
                    "DeepSeek Chat",
                    "deepseek-chat",
                    "2024-01-01T00:00:00Z",
                ),
            )
            .await?;
        assert_eq!(created.id, "m-1");
        assert_eq!(created.name, "DeepSeek Chat");
        assert_eq!(created.status, "disabled");

        let by_id = repo
            .find_model(&ctx, "m-1")
            .await?
            .ok_or(RepoError::NotFound("m-1"))?;
        assert_eq!(by_id.id, "m-1");

        let by_name = repo
            .find_model_by_name(&ctx, "DeepSeek Chat")
            .await?
            .ok_or(RepoError::NotFound("name"))?;
        assert_eq!(by_name.id, "m-1");

        let by_mid = repo
            .find_model_by_model_id(&ctx, "deepseek-chat")
            .await?
            .ok_or(RepoError::NotFound("model_id"))?;
        assert_eq!(by_mid.id, "m-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_model(&anon, "m-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_model(&anon, input("m-2", "Beta", "b", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn name_and_model_id_conflict_on_create_and_update() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();
        repo.create_model(
            &ctx,
            input("m-1", "Alpha", "alpha-1", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.create_model(&ctx, input("m-2", "Beta", "beta-1", "2024-01-02T00:00:00Z"))
            .await?;

        let dup_name = repo
            .create_model(
                &ctx,
                input("m-3", "Alpha", "gamma-1", "2024-01-03T00:00:00Z"),
            )
            .await;
        assert!(matches!(dup_name, Err(RepoError::NameConflict)));

        let dup_mid = repo
            .create_model(
                &ctx,
                input("m-4", "Gamma", "alpha-1", "2024-01-04T00:00:00Z"),
            )
            .await;
        assert!(matches!(dup_mid, Err(RepoError::NameConflict)));

        let rename = repo
            .update_model(
                &ctx,
                "m-2",
                UpdateModelInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-05T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::NameConflict)));

        let re_mid = repo
            .update_model(
                &ctx,
                "m-2",
                UpdateModelInput {
                    model_id: Some("alpha-1".into()),
                    updated_at: "2024-01-06T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(re_mid, Err(RepoError::NameConflict)));

        let self_rename = repo
            .update_model(
                &ctx,
                "m-1",
                UpdateModelInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-07T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(self_rename.name, "Alpha");
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_hides_row_from_default_queries() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();
        repo.create_model(
            &ctx,
            input("m-1", "Alpha", "alpha-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        let deleted = repo
            .soft_delete_model(&ctx, "m-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "archived");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_model(&ctx, "m-1").await?.is_none());
        assert!(repo.find_model_by_name(&ctx, "Alpha").await?.is_none());
        assert!(
            repo.find_model_by_model_id(&ctx, "alpha-1")
                .await?
                .is_none()
        );
        assert!(!repo.model_exists(&ctx, "Alpha").await?);
        assert!(repo.find_model_with_deleted(&ctx, "m-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn restore_brings_row_back_and_frees_keys() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();
        repo.create_model(
            &ctx,
            input("m-1", "Alpha", "alpha-1", "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.soft_delete_model(&ctx, "m-1", "2024-02-01T00:00:00Z")
            .await?;

        repo.create_model(
            &ctx,
            input("m-2", "Alpha", "alpha-1", "2024-03-01T00:00:00Z"),
        )
        .await?;

        let restore_conflict = repo.restore_model(&ctx, "m-1").await;
        assert!(matches!(restore_conflict, Err(RepoError::NameConflict)));

        repo.soft_delete_model(&ctx, "m-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_model(&ctx, "m-1").await?;
        assert_eq!(restored.status, "disabled");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_model(&ctx, "m-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_model(&ctx, input("c", "C", "c-id", ts)).await?;
        repo.create_model(&ctx, input("a", "A", "a-id", ts)).await?;
        repo.create_model(&ctx, input("b", "B", "b-id", ts)).await?;

        let page1 = repo
            .list_models(
                &ctx,
                &ListModelsQuery {
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
            .list_models(
                &ctx,
                &ListModelsQuery {
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
    async fn update_writes_model_card_and_settings_json() -> RepoResult<()> {
        let repo = InMemoryModelRepo::new();
        let ctx = ctx_allowed();
        repo.create_model(
            &ctx,
            input("m-1", "Alpha", "alpha-1", "2024-01-01T00:00:00Z"),
        )
        .await?;

        let updated = repo
            .update_model(
                &ctx,
                "m-1",
                UpdateModelInput {
                    status: Some("enabled".into()),
                    model_card: Some(serde_json::json!({"summary": "new"})),
                    settings: Some(serde_json::json!({"upstream": "v2"})),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, "enabled");
        assert_eq!(updated.model_card, serde_json::json!({"summary": "new"}));
        assert_eq!(updated.settings, serde_json::json!({"upstream": "v2"}));
        Ok(())
    }
}

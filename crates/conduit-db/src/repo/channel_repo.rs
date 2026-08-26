//! Channel repository — Rust port of `conduit/internal/server/biz/channel.go`.
//!
//! Mirrors the Go `ChannelService` data-access surface against the `Channel`
//! Ent schema (`internal/ent/schema/channel.go`): name-keyed lookup, id-keyed
//! lookup, soft delete via `deleted_at`, status transitions, paginated listing,
//! the `enabled` projection that backs the candidate-selection cache, and a
//! tag filter. Channels are **global** (not project-scoped).
//!
//! ## Storage model (RUST-P3-002 S13)
//! `ChannelRow` is now a hand-written typed struct carrying the real Go entity
//! columns. JSON columns (`credentials`, `disabled_api_keys`, `policies`,
//! `settings`, `endpoints`) are `serde_json::Value` / `Vec<Value>`.
//!
//! ## Uniqueness (mirrors Go `Channel.Indexes`)
//! `channels_by_name` is a unique index over `(name, deleted_at)`, paired with
//! the `SoftDeleteMixin`. Two soft-deleted rows may share a name with one live
//! row; a soft-deleted row frees its name for re-registration.
//!
//! ## Status semantics
//! Go enum: `enabled | disabled | archived`; default on create is `disabled`.
//! Soft delete sets `archived`.
//!
//! ## Cache signature
//! `cache_signature(row) = "{id}:{updated_atRFC3339}"`. There is no persisted
//! signature column in Go; this is a Rust-side cache-key convention.
//!
//! ## Pagination
//! `list_channels` orders by `created_at ASC` with a stable `id ASC` tie-breaker.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::ChannelRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when creating a channel. Mirrors the Go
/// `ent.CreateChannelInput` minus edges. `status` is server-managed (defaults
/// to `disabled`, matching the Go schema default).
#[derive(Debug, Clone)]
pub struct CreateChannelInput {
    pub id: String,
    pub channel_type: String,
    pub name: String,
    pub base_url: Option<String>,
    pub website_url: Option<String>,
    pub quota_currency: Option<String>,
    pub actual_quota_used: Option<String>,
    pub quota_remaining: Option<String>,
    pub credentials: Value,
    pub supported_models: Vec<String>,
    pub manual_models: Vec<String>,
    pub default_test_model: String,
    pub auto_sync_supported_models: bool,
    pub auto_sync_model_pattern: String,
    pub tags: Vec<String>,
    pub policies: Option<Value>,
    pub settings: Option<Value>,
    pub endpoints: Vec<Value>,
    pub remark: Option<String>,
    pub ordering_weight: i64,
    pub created_at: String,
}

/// Patch applied by `update_channel`. Only non-`None` fields are written.
#[derive(Debug, Default, Clone)]
pub struct UpdateChannelInput {
    pub channel_type: Option<String>,
    pub name: Option<String>,
    pub base_url: Option<Option<String>>,
    pub website_url: Option<Option<String>>,
    pub quota_currency: Option<Option<String>>,
    pub actual_quota_used: Option<Option<String>>,
    pub quota_remaining: Option<Option<String>>,
    pub credentials: Option<Value>,
    pub disabled_api_keys: Option<Value>,
    pub supported_models: Option<Vec<String>>,
    pub manual_models: Option<Vec<String>>,
    pub auto_sync_supported_models: Option<bool>,
    pub auto_sync_model_pattern: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub default_test_model: Option<String>,
    pub policies: Option<Value>,
    pub settings: Option<Value>,
    pub endpoints: Option<Vec<Value>>,
    pub remark: Option<Option<String>>,
    pub ordering_weight: Option<i64>,
    pub error_message: Option<Option<String>>,
    pub status: Option<String>,
    pub updated_at: String,
}

/// Pagination/filter params for `list_channels`.
#[derive(Debug, Clone)]
pub struct ListChannelsQuery {
    pub limit: u32,
    pub offset: u32,
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
    pub status_in: Vec<String>,
}

impl Default for ListChannelsQuery {
    fn default() -> Self {
        Self {
            limit: 20,
            offset: 0,
            after_created_at: None,
            after_id: None,
            status_in: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListChannelsResult {
    pub rows: Vec<ChannelRow>,
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

fn is_live(row: &ChannelRow) -> bool {
    row.deleted_at.is_none()
}

fn row_from_input(input: &CreateChannelInput) -> ChannelRow {
    let now = parse_dt(&input.created_at);
    ChannelRow {
        id: input.id.clone(),
        channel_type: input.channel_type.clone(),
        base_url: input.base_url.clone(),
        website_url: input.website_url.clone(),
        quota_currency: input.quota_currency.clone(),
        actual_quota_used: input.actual_quota_used.clone(),
        quota_remaining: input.quota_remaining.clone(),
        name: input.name.clone(),
        status: "disabled".into(),
        credentials: input.credentials.clone(),
        disabled_api_keys: Value::Array(Vec::new()),
        supported_models: input.supported_models.clone(),
        manual_models: input.manual_models.clone(),
        auto_sync_supported_models: input.auto_sync_supported_models,
        auto_sync_model_pattern: input.auto_sync_model_pattern.clone(),
        tags: input.tags.clone(),
        default_test_model: input.default_test_model.clone(),
        policies: input
            .policies
            .clone()
            .unwrap_or_else(|| json!({"stream": "unlimited"})),
        settings: input
            .settings
            .clone()
            .unwrap_or_else(|| json!({"model_mappings": []})),
        ordering_weight: input.ordering_weight,
        error_message: None,
        remark: input.remark.clone(),
        endpoints: input.endpoints.clone(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn apply_update(row: &mut ChannelRow, input: &UpdateChannelInput) {
    if let Some(channel_type) = &input.channel_type {
        row.channel_type = channel_type.clone();
    }
    if let Some(name) = &input.name {
        row.name = name.clone();
    }
    if let Some(base_url) = &input.base_url {
        row.base_url = base_url.clone();
    }
    if let Some(website_url) = &input.website_url {
        row.website_url = website_url.clone();
    }
    if let Some(quota_currency) = &input.quota_currency {
        row.quota_currency = quota_currency.clone();
    }
    if let Some(actual_quota_used) = &input.actual_quota_used {
        row.actual_quota_used = actual_quota_used.clone();
    }
    if let Some(quota_remaining) = &input.quota_remaining {
        row.quota_remaining = quota_remaining.clone();
    }
    if let Some(credentials) = &input.credentials {
        row.credentials = credentials.clone();
    }
    if let Some(disabled_api_keys) = &input.disabled_api_keys {
        row.disabled_api_keys = disabled_api_keys.clone();
    }
    if let Some(supported_models) = &input.supported_models {
        row.supported_models = supported_models.clone();
    }
    if let Some(manual_models) = &input.manual_models {
        row.manual_models = manual_models.clone();
    }
    if let Some(auto_sync) = input.auto_sync_supported_models {
        row.auto_sync_supported_models = auto_sync;
    }
    if let Some(pattern) = &input.auto_sync_model_pattern {
        row.auto_sync_model_pattern = pattern.clone().unwrap_or_default();
    }
    if let Some(tags) = &input.tags {
        row.tags = tags.clone();
    }
    if let Some(default_test_model) = &input.default_test_model {
        row.default_test_model = default_test_model.clone();
    }
    if let Some(policies) = &input.policies {
        row.policies = policies.clone();
    }
    if let Some(settings) = &input.settings {
        row.settings = settings.clone();
    }
    if let Some(endpoints) = &input.endpoints {
        row.endpoints = endpoints.clone();
    }
    if let Some(remark) = &input.remark {
        row.remark = remark.clone();
    }
    if let Some(weight) = &input.ordering_weight {
        row.ordering_weight = *weight;
    }
    if let Some(error_message) = &input.error_message {
        row.error_message = error_message.clone();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

/// Compute the cache signature for a channel row.
///
/// Derived from the row's stable identity (`id`) paired with its `updated_at`
/// (as RFC-3339), mirroring how Go's `ChannelService` detects reload needs.
pub fn cache_signature(row: &ChannelRow) -> String {
    format!("{}:{}", row.id, row.updated_at.to_rfc3339())
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait ChannelRepo: Send + Sync {
    async fn create_channel_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateChannelInput,
    ) -> RepoResult<ChannelRow>;

    async fn create_channel(
        &self,
        ctx: &RequestContext,
        input: CreateChannelInput,
    ) -> RepoResult<ChannelRow> {
        guard_repo_principal(ctx)?;
        self.create_channel_unchecked(ctx, input).await
    }

    async fn find_channel_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>>;

    async fn find_channel(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.find_channel_unchecked(ctx, channel_id).await
    }

    async fn find_channel_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>>;

    async fn find_channel_with_deleted(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.find_channel_with_deleted_unchecked(ctx, channel_id)
            .await
    }

    async fn find_channel_by_name_unchecked(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ChannelRow>>;

    async fn find_channel_by_name(
        &self,
        ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.find_channel_by_name_unchecked(ctx, name).await
    }

    async fn find_channels_by_tags_unchecked(
        &self,
        ctx: &RequestContext,
        tags: &[String],
    ) -> RepoResult<Vec<ChannelRow>>;

    async fn find_channels_by_tags(
        &self,
        ctx: &RequestContext,
        tags: &[String],
    ) -> RepoResult<Vec<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.find_channels_by_tags_unchecked(ctx, tags).await
    }

    async fn list_enabled_channels_unchecked(
        &self,
        ctx: &RequestContext,
    ) -> RepoResult<Vec<ChannelRow>>;

    async fn list_enabled_channels(&self, ctx: &RequestContext) -> RepoResult<Vec<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.list_enabled_channels_unchecked(ctx).await
    }

    async fn find_channel_by_cache_signature_unchecked(
        &self,
        ctx: &RequestContext,
        cache_signature: &str,
    ) -> RepoResult<Option<ChannelRow>>;

    async fn find_channel_by_cache_signature(
        &self,
        ctx: &RequestContext,
        cache_signature: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        guard_repo_principal(ctx)?;
        self.find_channel_by_cache_signature_unchecked(ctx, cache_signature)
            .await
    }

    async fn list_channels_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListChannelsQuery,
    ) -> RepoResult<ListChannelsResult>;

    async fn list_channels(
        &self,
        ctx: &RequestContext,
        query: &ListChannelsQuery,
    ) -> RepoResult<ListChannelsResult> {
        guard_repo_principal(ctx)?;
        self.list_channels_unchecked(ctx, query).await
    }

    async fn update_channel_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
        input: UpdateChannelInput,
    ) -> RepoResult<ChannelRow>;

    async fn update_channel(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
        input: UpdateChannelInput,
    ) -> RepoResult<ChannelRow> {
        guard_repo_principal(ctx)?;
        self.update_channel_unchecked(ctx, channel_id, input).await
    }

    async fn soft_delete_channel_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ChannelRow>;

    async fn soft_delete_channel(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ChannelRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_channel_unchecked(ctx, channel_id, deleted_at)
            .await
    }

    async fn restore_channel_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<ChannelRow>;

    async fn restore_channel(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<ChannelRow> {
        guard_repo_principal(ctx)?;
        self.restore_channel_unchecked(ctx, channel_id).await
    }

    async fn channel_exists_unchecked(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool>;

    async fn channel_exists(&self, ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.channel_exists_unchecked(ctx, name).await
    }
}

// --- in-memory implementation ----------------------------------------------

#[derive(Debug, Default)]
pub struct InMemoryChannelRepo {
    rows: Mutex<BTreeMap<String, ChannelRow>>,
}

impl InMemoryChannelRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = ChannelRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    fn name_in_use_locked(
        guard: &BTreeMap<String, ChannelRow>,
        name: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| is_live(row) && row.name == name && Some(row.id.as_str()) != exclude_id)
    }

    /// DESC by weight, ASC by name.
    fn order_by_weight_then_name(a: &ChannelRow, b: &ChannelRow) -> std::cmp::Ordering {
        b.ordering_weight
            .cmp(&a.ordering_weight)
            .then_with(|| a.name.cmp(&b.name))
    }
}

#[async_trait]
impl ChannelRepo for InMemoryChannelRepo {
    async fn create_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateChannelInput,
    ) -> RepoResult<ChannelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;
        if Self::name_in_use_locked(&guard, &input.name, None) {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("channel id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
            .get(channel_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_channel_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
            .get(channel_id)
            .cloned())
    }

    async fn find_channel_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
            .values()
            .find(|r| is_live(r) && r.name == name)
            .cloned())
    }

    async fn find_channels_by_tags_unchecked(
        &self,
        _ctx: &RequestContext,
        tags: &[String],
    ) -> RepoResult<Vec<ChannelRow>> {
        if tags.is_empty() {
            return Ok(Vec::new());
        }
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;
        let mut hits: Vec<ChannelRow> = guard
            .values()
            .filter(|r| is_live(r))
            .filter(|r| tags.iter().any(|t| r.tags.iter().any(|rt| rt == t)))
            .cloned()
            .collect();
        hits.sort_by(Self::order_by_weight_then_name);
        Ok(hits)
    }

    async fn list_enabled_channels_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<ChannelRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;
        let mut enabled: Vec<ChannelRow> = guard
            .values()
            .filter(|r| is_live(r) && r.status == "enabled")
            .cloned()
            .collect();
        enabled.sort_by(Self::order_by_weight_then_name);
        Ok(enabled)
    }

    async fn find_channel_by_cache_signature_unchecked(
        &self,
        _ctx: &RequestContext,
        signature: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
            .values()
            .find(|r| cache_signature(r) == signature)
            .cloned())
    }

    async fn list_channels_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListChannelsQuery,
    ) -> RepoResult<ListChannelsResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;

        let mut live: Vec<ChannelRow> = guard
            .values()
            .filter(|r| is_live(r))
            .filter(|r| {
                query.status_in.is_empty() || query.status_in.iter().any(|s| s == &r.status)
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

        Ok(ListChannelsResult { rows, has_more })
    }

    async fn update_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
        input: UpdateChannelInput,
    ) -> RepoResult<ChannelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;

        if let Some(new_name) = &input.name
            && Self::name_in_use_locked(&guard, new_name, Some(channel_id))
        {
            return Err(RepoError::NameConflict);
        }

        let row = guard
            .get_mut(channel_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("channel"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ChannelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;
        let row = guard
            .get_mut(channel_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("channel"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        row.status = "archived".into();
        Ok(row.clone())
    }

    async fn restore_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<ChannelRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?;

        let (name, is_deleted) = guard
            .get(channel_id)
            .map(|r| (r.name.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("channel"))?;

        if is_deleted {
            if Self::name_in_use_locked(&guard, &name, Some(channel_id)) {
                return Err(RepoError::NameConflict);
            }
            let row = guard
                .get_mut(channel_id)
                .ok_or(RepoError::NotFound("channel"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "disabled".into();
        }
        Ok(guard
            .get(channel_id)
            .ok_or(RepoError::NotFound("channel"))?
            .clone())
    }

    async fn channel_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("channel repo"))?
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

    fn input(id: &str, name: &str, created_at: &str) -> CreateChannelInput {
        CreateChannelInput {
            id: id.into(),
            channel_type: "openai".into(),
            name: name.into(),
            base_url: Some("https://api.openai.com".into()),
            website_url: None,
            quota_currency: Some("USD".into()),
            actual_quota_used: None,
            quota_remaining: None,
            credentials: json!({"api_key": "sk-test"}),
            supported_models: vec!["gpt-4o".into()],
            manual_models: Vec::new(),
            default_test_model: "gpt-4o".into(),
            auto_sync_supported_models: false,
            auto_sync_model_pattern: String::new(),
            tags: Vec::new(),
            policies: None,
            settings: None,
            endpoints: Vec::new(),
            remark: None,
            ordering_weight: 0,
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id_and_name() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_channel(&ctx, input("c-1", "Primary", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "c-1");
        assert_eq!(created.name, "Primary");
        assert_eq!(created.status, "disabled");
        assert_eq!(created.channel_type, "openai");
        assert_eq!(created.credentials, json!({"api_key": "sk-test"}));
        // Defaults applied when caller omits policies/settings.
        assert_eq!(created.policies, json!({"stream": "unlimited"}));
        assert_eq!(created.settings, json!({"model_mappings": []}));

        let by_id = repo
            .find_channel(&ctx, "c-1")
            .await?
            .ok_or(RepoError::NotFound("c-1"))?;
        assert_eq!(by_id.id, "c-1");

        let by_name = repo
            .find_channel_by_name(&ctx, "Primary")
            .await?
            .ok_or(RepoError::NotFound("name"))?;
        assert_eq!(by_name.id, "c-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_channel(&anon, "c-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_channel(&anon, input("c-2", "Beta", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn name_conflict_on_create_and_update() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_channel(&ctx, input("c-2", "Beta", "2024-01-02T00:00:00Z"))
            .await?;

        let dup = repo
            .create_channel(&ctx, input("c-3", "Alpha", "2024-01-03T00:00:00Z"))
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));

        let rename = repo
            .update_channel(
                &ctx,
                "c-2",
                UpdateChannelInput {
                    name: Some("Alpha".into()),
                    updated_at: "2024-01-04T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::NameConflict)));

        let self_rename = repo
            .update_channel(
                &ctx,
                "c-1",
                UpdateChannelInput {
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
    async fn list_enabled_filters_out_disabled_and_archived() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Disabled", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_channel(&ctx, input("c-2", "Enabled", "2024-01-02T00:00:00Z"))
            .await?;
        repo.update_channel(
            &ctx,
            "c-2",
            UpdateChannelInput {
                status: Some("enabled".into()),
                updated_at: "2024-01-03T00:00:00Z".into(),
                ..Default::default()
            },
        )
        .await?;
        repo.create_channel(&ctx, input("c-3", "Gone", "2024-01-04T00:00:00Z"))
            .await?;
        repo.update_channel(
            &ctx,
            "c-3",
            UpdateChannelInput {
                status: Some("enabled".into()),
                updated_at: "2024-01-05T00:00:00Z".into(),
                ..Default::default()
            },
        )
        .await?;
        repo.soft_delete_channel(&ctx, "c-3", "2024-02-01T00:00:00Z")
            .await?;

        let enabled = repo.list_enabled_channels(&ctx).await?;
        let ids: Vec<_> = enabled.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["c-2"]);
        Ok(())
    }

    #[tokio::test]
    async fn list_enabled_orders_by_weight_desc_then_name() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        for (id, name) in [("c-3", "Cee"), ("c-1", "Aye"), ("c-2", "Bee")] {
            repo.create_channel(&ctx, input(id, name, "2024-01-01T00:00:00Z"))
                .await?;
            repo.update_channel(
                &ctx,
                id,
                UpdateChannelInput {
                    status: Some("enabled".into()),
                    updated_at: "2024-01-02T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        }
        repo.update_channel(
            &ctx,
            "c-2",
            UpdateChannelInput {
                ordering_weight: Some(10),
                updated_at: "2024-01-03T00:00:00Z".into(),
                ..Default::default()
            },
        )
        .await?;

        let enabled = repo.list_enabled_channels(&ctx).await?;
        let names: Vec<_> = enabled.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Bee", "Aye", "Cee"]);
        Ok(())
    }

    #[tokio::test]
    async fn find_by_tags_returns_any_match_ordered() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();

        let mut i1 = input("c-1", "Alpha", "2024-01-01T00:00:00Z");
        i1.tags = vec!["prod".into(), "fast".into()];
        let mut i2 = input("c-2", "Beta", "2024-01-02T00:00:00Z");
        i2.tags = vec!["dev".into()];
        i2.ordering_weight = 5;
        let mut i3 = input("c-3", "Gamma", "2024-01-03T00:00:00Z");
        i3.tags = vec!["prod".into()];
        repo.create_channel(&ctx, i1).await?;
        repo.create_channel(&ctx, i2).await?;
        repo.create_channel(&ctx, i3).await?;

        let prod = repo.find_channels_by_tags(&ctx, &["prod".into()]).await?;
        let names: Vec<_> = prod.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Gamma"]);

        let mixed = repo
            .find_channels_by_tags(&ctx, &["prod".into(), "dev".into()])
            .await?;
        let names2: Vec<_> = mixed.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names2, vec!["Beta", "Alpha", "Gamma"]);

        let empty = repo.find_channels_by_tags(&ctx, &[]).await?;
        assert!(empty.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_then_restore_round_trip() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let deleted = repo
            .soft_delete_channel(&ctx, "c-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "archived");
        assert!(deleted.deleted_at.is_some());

        assert!(repo.find_channel(&ctx, "c-1").await?.is_none());
        assert!(repo.find_channel_by_name(&ctx, "Alpha").await?.is_none());
        assert!(!repo.channel_exists(&ctx, "Alpha").await?);
        assert!(repo.find_channel_with_deleted(&ctx, "c-1").await?.is_some());

        repo.create_channel(&ctx, input("c-2", "Alpha", "2024-03-01T00:00:00Z"))
            .await?;

        let restore_conflict = repo.restore_channel(&ctx, "c-1").await;
        assert!(matches!(restore_conflict, Err(RepoError::NameConflict)));

        repo.soft_delete_channel(&ctx, "c-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_channel(&ctx, "c-1").await?;
        assert_eq!(restored.status, "disabled");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_channel(&ctx, "c-1").await?.is_some());

        let again = repo.restore_channel(&ctx, "c-1").await?;
        assert_eq!(again.status, "disabled");
        Ok(())
    }

    #[tokio::test]
    async fn cache_signature_changes_with_updated_at() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let before = repo
            .find_channel(&ctx, "c-1")
            .await?
            .ok_or(RepoError::NotFound("c-1"))?;
        let sig_before = cache_signature(&before);
        assert!(sig_before.starts_with("c-1:"));

        repo.update_channel(
            &ctx,
            "c-1",
            UpdateChannelInput {
                status: Some("enabled".into()),
                updated_at: "2024-01-02T00:00:00Z".into(),
                ..Default::default()
            },
        )
        .await?;
        let after = repo
            .find_channel(&ctx, "c-1")
            .await?
            .ok_or(RepoError::NotFound("c-1"))?;
        let sig_after = cache_signature(&after);
        assert_ne!(sig_before, sig_after);

        let resolved = repo
            .find_channel_by_cache_signature(&ctx, &sig_after)
            .await?
            .ok_or(RepoError::NotFound("sig"))?;
        assert_eq!(resolved.id, "c-1");

        let stale = repo
            .find_channel_by_cache_signature(&ctx, &sig_before)
            .await?;
        assert!(stale.is_none());

        repo.soft_delete_channel(&ctx, "c-1", "2024-03-01T00:00:00Z")
            .await?;
        let sig_deleted = cache_signature(
            &repo
                .find_channel_with_deleted(&ctx, "c-1")
                .await?
                .ok_or(RepoError::NotFound("archived"))?,
        );
        let archived = repo
            .find_channel_by_cache_signature(&ctx, &sig_deleted)
            .await?
            .ok_or(RepoError::NotFound("archived sig"))?;
        assert_eq!(archived.status, "archived");
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_channel(&ctx, input("c", "Cee", ts)).await?;
        repo.create_channel(&ctx, input("a", "Aye", ts)).await?;
        repo.create_channel(&ctx, input("b", "Bee", ts)).await?;

        let page1 = repo
            .list_channels(
                &ctx,
                &ListChannelsQuery {
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
            .list_channels(
                &ctx,
                &ListChannelsQuery {
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
    async fn list_filters_by_status_in() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_channel(&ctx, input("c-2", "Beta", "2024-01-02T00:00:00Z"))
            .await?;
        repo.update_channel(
            &ctx,
            "c-2",
            UpdateChannelInput {
                status: Some("enabled".into()),
                updated_at: "2024-01-03T00:00:00Z".into(),
                ..Default::default()
            },
        )
        .await?;

        let enabled = repo
            .list_channels(
                &ctx,
                &ListChannelsQuery {
                    status_in: vec!["enabled".into()],
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = enabled.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["c-2"]);

        let all = repo
            .list_channels(&ctx, &ListChannelsQuery::default())
            .await?;
        assert_eq!(all.rows.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn update_writes_json_columns_and_clears_nullable_scalars() -> RepoResult<()> {
        let repo = InMemoryChannelRepo::new();
        let ctx = ctx_allowed();
        repo.create_channel(&ctx, input("c-1", "Alpha", "2024-01-01T00:00:00Z"))
            .await?;

        let updated = repo
            .update_channel(
                &ctx,
                "c-1",
                UpdateChannelInput {
                    credentials: Some(json!({"api_key": "sk-new"})),
                    settings: Some(json!({"model_mappings": [{"from": "a", "to": "b"}]})),
                    endpoints: Some(vec![json!({"api_format": "openai"})]),
                    policies: Some(json!({"stream": "disabled"})),
                    disabled_api_keys: Some(json!([{"key_id": "k1"}])),
                    remark: Some(Some("note".into())),
                    error_message: Some(Some("boom".into())),
                    ordering_weight: Some(42),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.credentials, json!({"api_key": "sk-new"}));
        assert_eq!(
            updated.settings,
            json!({"model_mappings": [{"from": "a", "to": "b"}]})
        );
        assert_eq!(updated.endpoints, vec![json!({"api_format": "openai"})]);
        assert_eq!(updated.policies, json!({"stream": "disabled"}));
        assert_eq!(updated.disabled_api_keys, json!([{"key_id": "k1"}]));
        assert_eq!(updated.remark, Some("note".into()));
        assert_eq!(updated.error_message, Some("boom".into()));
        assert_eq!(updated.ordering_weight, 42);

        let cleared = repo
            .update_channel(
                &ctx,
                "c-1",
                UpdateChannelInput {
                    remark: Some(None),
                    error_message: Some(None),
                    updated_at: "2024-03-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(cleared.remark.is_none());
        assert!(cleared.error_message.is_none());
        Ok(())
    }
}

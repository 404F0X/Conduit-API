//! User repository — Rust port of `conduit/internal/server/biz/user.go`.
//!
//! Implements the read/write surface the Go `UserService` exposes against the
//! `User` Ent schema (`internal/ent/schema/user.go`): email-keyed lookup, soft
//! delete via a `deleted_at` timestamp, status transitions, and paginated
//! listing.
//!
//! ## Storage model (RUST-P3-002 S13 pilot)
//! `UserRow` is now a hand-written typed struct carrying the real Go entity
//! columns (email, status, prefer_language, first/last_name, avatar, is_owner,
//! scopes, created_at, updated_at, deleted_at). The previous `minimal_row!`
//! generic shape (with its stringly-typed `extra` map) has been retired.
//!
//! ## Soft delete (RUST-P3-004 S13)
//! Every default query filters `deleted_at IS NULL`. Methods suffixed
//! `_with_deleted` skip the filter for restore/admin flows; `*_system_bypass`
//! additionally skips the policy guard for internal callers. This mirrors the
//! Go `SoftDeleteMixin` on the `User` schema.
//!
//! ## Pagination (RUST-P3-004 S15)
//! `list_users` orders by `created_at ASC` with a stable `id ASC` tie-breaker
//! so equal timestamps don't reshuffle across pages.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::UserRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when creating a user. Mirrors the Go
/// `ent.CreateUserInput` shape minus edges (roles/projects/oidc_identities),
/// which land with their own repos.
#[derive(Debug, Clone)]
pub struct CreateUserInput {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub prefer_language: Option<String>,
    pub avatar: Option<String>,
    pub is_owner: bool,
    pub scopes: Vec<String>,
    /// Caller-supplied timestamp (epoch millis or ISO-8601). Parsed into a
    /// `DateTime<Utc>` on the row so ordering is type-safe.
    pub created_at: String,
}

/// Patch applied by `update_user`. Only non-`None` fields are written.
#[derive(Debug, Default, Clone)]
pub struct UpdateUserInput {
    pub email: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub prefer_language: Option<String>,
    pub avatar: Option<Option<String>>,
    pub is_owner: Option<bool>,
    pub scopes: Option<Vec<String>>,
    pub status: Option<String>,
    /// New `updated_at` value; required by the soft-delete mixin contract.
    /// Parsed into `DateTime<Utc>` on the row.
    pub updated_at: String,
}

/// Pagination/filter params for `list_users`. `created_after` enables
/// keyset-style pagination on top of the limit/offset window.
#[derive(Debug, Clone)]
pub struct ListUsersQuery {
    pub limit: u32,
    pub offset: u32,
    /// Inclusive lower bound on `(created_at, id)` for keyset pagination.
    pub after_created_at: Option<String>,
    pub after_id: Option<String>,
}

impl Default for ListUsersQuery {
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
pub struct ListUsersResult {
    pub rows: Vec<UserRow>,
    /// True when another page likely exists (caller may bump `offset`/keyset).
    pub has_more: bool,
}

// --- timestamp parsing -----------------------------------------------------

/// Parse an ISO-8601 / RFC-3339 / date-only string into `DateTime<Utc>`.
/// Falls back to the Unix epoch on failure so comparisons don't panic —
/// well-formed inputs are unaffected.
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

/// Live = not soft-deleted. The single source of truth for the
/// `deleted_at IS NULL` filter.
fn is_live(row: &UserRow) -> bool {
    row.deleted_at.is_none()
}

/// Build a `UserRow` from creation input.
fn row_from_input(input: &CreateUserInput) -> UserRow {
    let now = parse_dt(&input.created_at);
    UserRow {
        id: input.id.clone(),
        email: input.email.clone(),
        status: "activated".into(),
        prefer_language: input.prefer_language.clone().unwrap_or_else(|| "en".into()),
        first_name: input.first_name.clone().unwrap_or_default(),
        last_name: input.last_name.clone().unwrap_or_default(),
        avatar: input.avatar.clone(),
        is_owner: input.is_owner,
        scopes: input.scopes.clone(),
        created_at: now,
        updated_at: now,
        deleted_at: None,
        // password_hash is intentionally NOT stored on the repository row;
        // hashing stays in the auth layer. The Go service keeps it on the Ent
        // entity, but exposing it through a read repo would be a credential leak.
    }
}

/// Apply `UpdateUserInput` to a row in place. Only touches fields the caller
/// set; `updated_at` is always written.
fn apply_update(row: &mut UserRow, input: &UpdateUserInput) {
    if let Some(email) = &input.email {
        row.email = email.clone();
    }
    if let Some(first) = &input.first_name {
        row.first_name = first.clone();
    }
    if let Some(last) = &input.last_name {
        row.last_name = last.clone();
    }
    if let Some(lang) = &input.prefer_language {
        row.prefer_language = lang.clone();
    }
    if let Some(avatar) = &input.avatar {
        row.avatar = avatar.clone();
    }
    if let Some(owner) = input.is_owner {
        row.is_owner = owner;
    }
    if let Some(scopes) = &input.scopes {
        row.scopes = scopes.clone();
    }
    if let Some(status) = &input.status {
        row.status = status.clone();
    }
    row.updated_at = parse_dt(&input.updated_at);
}

// --- trait -----------------------------------------------------------------

/// Repository surface for the `users` table.
///
/// Method naming convention (RUST-P3-004 S13):
/// - `find_user_*` / `list_users` — default scope, **excludes soft-deleted** rows.
/// - `*_with_deleted` — same data, includes soft-deleted rows (restore/admin).
/// - `*_unchecked` — skips the policy guard; callers must have already enforced
///   authorization. Public methods wrap these with `guard_repo_principal`.
#[async_trait]
pub trait UserRepo: Send + Sync {
    // --- create ---
    async fn create_user_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateUserInput,
    ) -> RepoResult<UserRow>;

    async fn create_user(
        &self,
        ctx: &RequestContext,
        input: CreateUserInput,
    ) -> RepoResult<UserRow> {
        guard_repo_principal(ctx)?;
        self.create_user_unchecked(ctx, input).await
    }

    // --- read: by id ---
    async fn find_user_by_id_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>>;

    async fn find_user_by_id(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        guard_repo_principal(ctx)?;
        self.find_user_by_id_unchecked(ctx, user_id).await
    }

    /// Includes soft-deleted rows. Intended for restore flows and audit.
    async fn find_user_by_id_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>>;

    async fn find_user_by_id_with_deleted(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        guard_repo_principal(ctx)?;
        self.find_user_by_id_with_deleted_unchecked(ctx, user_id)
            .await
    }

    // --- read: by email ---
    async fn find_user_by_email_unchecked(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>>;

    async fn find_user_by_email(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        guard_repo_principal(ctx)?;
        self.find_user_by_email_unchecked(ctx, email).await
    }

    async fn find_user_by_email_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>>;

    async fn find_user_by_email_with_deleted(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        guard_repo_principal(ctx)?;
        self.find_user_by_email_with_deleted_unchecked(ctx, email)
            .await
    }

    // --- list (paginated) ---
    async fn list_users_unchecked(
        &self,
        ctx: &RequestContext,
        query: &ListUsersQuery,
    ) -> RepoResult<ListUsersResult>;

    async fn list_users(
        &self,
        ctx: &RequestContext,
        query: &ListUsersQuery,
    ) -> RepoResult<ListUsersResult> {
        guard_repo_principal(ctx)?;
        self.list_users_unchecked(ctx, query).await
    }

    // --- update ---
    async fn update_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        input: UpdateUserInput,
    ) -> RepoResult<UserRow>;

    async fn update_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        input: UpdateUserInput,
    ) -> RepoResult<UserRow> {
        guard_repo_principal(ctx)?;
        self.update_user_unchecked(ctx, user_id, input).await
    }

    // --- soft delete / restore ---
    async fn soft_delete_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        deleted_at: &str,
    ) -> RepoResult<UserRow>;

    async fn soft_delete_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
        deleted_at: &str,
    ) -> RepoResult<UserRow> {
        guard_repo_principal(ctx)?;
        self.soft_delete_user_unchecked(ctx, user_id, deleted_at)
            .await
    }

    async fn restore_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<UserRow>;

    async fn restore_user(&self, ctx: &RequestContext, user_id: &str) -> RepoResult<UserRow> {
        guard_repo_principal(ctx)?;
        self.restore_user_unchecked(ctx, user_id).await
    }

    // --- existence ---
    async fn user_exists_unchecked(&self, ctx: &RequestContext, email: &str) -> RepoResult<bool>;

    async fn user_exists(&self, ctx: &RequestContext, email: &str) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.user_exists_unchecked(ctx, email).await
    }

    async fn user_exists_with_deleted_unchecked(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<bool>;

    async fn user_exists_with_deleted(
        &self,
        ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<bool> {
        guard_repo_principal(ctx)?;
        self.user_exists_with_deleted_unchecked(ctx, email).await
    }
}

// --- in-memory implementation ----------------------------------------------

/// In-memory `UserRepo` for unit tests and the bootstrap runtime before sqlx
/// lands. Storage is keyed by `id`; email uniqueness (live rows only) is
/// enforced to mirror Go's `(email, deleted_at)` unique index.
#[derive(Debug, Default)]
pub struct InMemoryUserRepo {
    rows: Mutex<BTreeMap<String, UserRow>>,
}

impl InMemoryUserRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = UserRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }

    /// True when a *live* row already owns `email`. The backing enforcement for
    /// the Go `(email, deleted_at)` unique index — a soft-deleted row frees its
    /// email for re-registration.
    fn email_in_use_locked(
        guard: &BTreeMap<String, UserRow>,
        email: &str,
        exclude_id: Option<&str>,
    ) -> bool {
        guard
            .values()
            .any(|row| is_live(row) && row.email == email && Some(row.id.as_str()) != exclude_id)
    }
}

#[async_trait]
impl UserRepo for InMemoryUserRepo {
    async fn create_user_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateUserInput,
    ) -> RepoResult<UserRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?;
        if Self::email_in_use_locked(&guard, &input.email, None) {
            return Err(RepoError::EmailConflict);
        }
        if guard.contains_key(&input.id) {
            // ID collision is a programmer error in the seeding path; surface
            // it as NotFound's sibling rather than panic.
            return Err(RepoError::NotFound("user id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_user_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .get(user_id)
            .filter(|r| is_live(r))
            .cloned())
    }

    async fn find_user_by_id_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Option<UserRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .get(user_id)
            .cloned())
    }

    async fn find_user_by_email_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .values()
            .find(|r| is_live(r) && r.email == email)
            .cloned())
    }

    async fn find_user_by_email_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<Option<UserRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .values()
            .find(|r| r.email == email)
            .cloned())
    }

    async fn list_users_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListUsersQuery,
    ) -> RepoResult<ListUsersResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?;

        // Collect live rows, then sort by (created_at, id) for a stable order
        // regardless of BTreeMap's id-keyed iteration.
        let mut live: Vec<UserRow> = guard.values().filter(|r| is_live(r)).cloned().collect();

        live.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        // Apply keyset filter when supplied: keep rows strictly after
        // (after_created_at, after_id). Falls back to plain offset otherwise.
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

        Ok(ListUsersResult { rows, has_more })
    }

    async fn update_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        input: UpdateUserInput,
    ) -> RepoResult<UserRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?;

        // Email uniqueness must be checked against the post-update state, so we
        // validate before mutating (and exclude the row itself).
        if let Some(new_email) = &input.email
            && Self::email_in_use_locked(&guard, new_email, Some(user_id))
        {
            return Err(RepoError::EmailConflict);
        }

        let row = guard
            .get_mut(user_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("user"))?;
        apply_update(row, &input);
        Ok(row.clone())
    }

    async fn soft_delete_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        deleted_at: &str,
    ) -> RepoResult<UserRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?;
        let row = guard
            .get_mut(user_id)
            .filter(|r| is_live(r))
            .ok_or(RepoError::NotFound("user"))?;
        let ts = parse_dt(deleted_at);
        row.deleted_at = Some(ts);
        row.updated_at = ts;
        // Mirrors Go's `deactivated` status on delete; the Go enum uses the
        // exact string `deactivated` so we keep it byte-identical.
        row.status = "deactivated".into();
        Ok(row.clone())
    }

    async fn restore_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<UserRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?;

        // Snapshot the email + soft-delete state before the mutable borrow
        // taken below; the conflict check needs an immutable view of the map.
        let (email, is_deleted) = guard
            .get(user_id)
            .map(|r| (r.email.clone(), r.deleted_at.is_some()))
            .ok_or(RepoError::NotFound("user"))?;

        // Restoring a live row is a no-op rather than an error — matches the Go
        // service's idempotent admin tooling.
        if is_deleted {
            // Re-check email doesn't collide with a newer live row that took the
            // address after this one was soft-deleted.
            if Self::email_in_use_locked(&guard, &email, Some(user_id)) {
                return Err(RepoError::EmailConflict);
            }
            let row = guard.get_mut(user_id).ok_or(RepoError::NotFound("user"))?;
            row.deleted_at = None;
            row.updated_at = Utc::now();
            row.status = "activated".into();
        }
        Ok(guard
            .get(user_id)
            .ok_or(RepoError::NotFound("user"))?
            .clone())
    }

    async fn user_exists_unchecked(&self, _ctx: &RequestContext, email: &str) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .values()
            .any(|r| is_live(r) && r.email == email))
    }

    async fn user_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        email: &str,
    ) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user repo"))?
            .values()
            .any(|r| r.email == email))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::RequestContext;

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn input(id: &str, email: &str, created_at: &str) -> CreateUserInput {
        CreateUserInput {
            id: id.into(),
            email: email.into(),
            password_hash: "hashed".into(),
            first_name: Some("Ada".into()),
            last_name: Some("Lovelace".into()),
            prefer_language: Some("en".into()),
            avatar: None,
            is_owner: false,
            scopes: vec!["read".into()],
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id_and_email() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_user(&ctx, input("u-1", "ada@x.io", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "u-1");
        assert_eq!(created.email, "ada@x.io");

        let by_id = repo
            .find_user_by_id(&ctx, "u-1")
            .await?
            .ok_or(RepoError::NotFound("u-1"))?;
        assert_eq!(by_id.id, "u-1");

        let by_email = repo
            .find_user_by_email(&ctx, "ada@x.io")
            .await?
            .ok_or(RepoError::NotFound("email"))?;
        assert_eq!(by_email.id, "u-1");
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_user_by_id(&anon, "u-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        // create is also guarded.
        let denied_create = repo
            .create_user(&anon, input("u-2", "b@x.io", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        assert_eq!(repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn email_conflict_on_create_and_update() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        repo.create_user(&ctx, input("u-1", "ada@x.io", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_user(&ctx, input("u-2", "charles@x.io", "2024-01-02T00:00:00Z"))
            .await?;

        // Duplicate email -> conflict.
        let dup = repo
            .create_user(&ctx, input("u-3", "ada@x.io", "2024-01-03T00:00:00Z"))
            .await;
        assert!(matches!(dup, Err(RepoError::EmailConflict)));

        // Renaming u-2 onto u-1's email -> conflict.
        let rename = repo
            .update_user(
                &ctx,
                "u-2",
                UpdateUserInput {
                    email: Some("ada@x.io".into()),
                    updated_at: "2024-01-04T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(rename, Err(RepoError::EmailConflict)));

        // Self-rename (same email) is allowed.
        let self_rename = repo
            .update_user(
                &ctx,
                "u-1",
                UpdateUserInput {
                    email: Some("ada@x.io".into()),
                    updated_at: "2024-01-05T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(self_rename.email, "ada@x.io");
        Ok(())
    }

    #[tokio::test]
    async fn soft_delete_hides_row_from_default_queries() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        repo.create_user(&ctx, input("u-1", "ada@x.io", "2024-01-01T00:00:00Z"))
            .await?;

        let deleted = repo
            .soft_delete_user(&ctx, "u-1", "2024-02-01T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "deactivated");
        assert!(deleted.deleted_at.is_some());

        // Default finders return None.
        assert!(repo.find_user_by_id(&ctx, "u-1").await?.is_none());
        assert!(repo.find_user_by_email(&ctx, "ada@x.io").await?.is_none());
        assert!(!repo.user_exists(&ctx, "ada@x.io").await?);

        // with_deleted variants still see it.
        assert!(
            repo.find_user_by_id_with_deleted(&ctx, "u-1")
                .await?
                .is_some()
        );
        assert!(repo.user_exists_with_deleted(&ctx, "ada@x.io").await?);
        Ok(())
    }

    #[tokio::test]
    async fn restore_brings_row_back_and_frees_email() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        repo.create_user(&ctx, input("u-1", "ada@x.io", "2024-01-01T00:00:00Z"))
            .await?;
        repo.soft_delete_user(&ctx, "u-1", "2024-02-01T00:00:00Z")
            .await?;

        // The freed email can be re-registered by a fresh row.
        repo.create_user(&ctx, input("u-2", "ada@x.io", "2024-03-01T00:00:00Z"))
            .await?;

        // Restoring the original now collides with u-2.
        let restore_conflict = repo.restore_user(&ctx, "u-1").await;
        assert!(matches!(restore_conflict, Err(RepoError::EmailConflict)));

        // Delete the squatter, then restore succeeds.
        repo.soft_delete_user(&ctx, "u-2", "2024-04-01T00:00:00Z")
            .await?;
        let restored = repo.restore_user(&ctx, "u-1").await?;
        assert_eq!(restored.status, "activated");
        assert!(restored.deleted_at.is_none());
        assert!(repo.find_user_by_id(&ctx, "u-1").await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn pagination_is_stable_across_equal_timestamps() -> RepoResult<()> {
        // Three users share created_at; order must be by id as tie-breaker.
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        let ts = "2024-01-01T00:00:00Z";
        repo.create_user(&ctx, input("c", "c@x.io", ts)).await?;
        repo.create_user(&ctx, input("a", "a@x.io", ts)).await?;
        repo.create_user(&ctx, input("b", "b@x.io", ts)).await?;

        let page1 = repo
            .list_users(
                &ctx,
                &ListUsersQuery {
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
            .list_users(
                &ctx,
                &ListUsersQuery {
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
    async fn keyset_pagination_after_cursor() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        repo.create_user(&ctx, input("a", "a@x.io", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_user(&ctx, input("b", "b@x.io", "2024-01-02T00:00:00Z"))
            .await?;
        repo.create_user(&ctx, input("c", "c@x.io", "2024-01-03T00:00:00Z"))
            .await?;

        // Resume strictly after (2024-01-02, b).
        let next = repo
            .list_users(
                &ctx,
                &ListUsersQuery {
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
    async fn update_unknown_user_is_not_found() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        let err = repo
            .update_user(
                &ctx,
                "ghost",
                UpdateUserInput {
                    updated_at: "2024-01-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(err, Err(RepoError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn update_cannot_mutate_soft_deleted_row() -> RepoResult<()> {
        let repo = InMemoryUserRepo::new();
        let ctx = ctx_allowed();
        repo.create_user(&ctx, input("u-1", "a@x.io", "2024-01-01T00:00:00Z"))
            .await?;
        repo.soft_delete_user(&ctx, "u-1", "2024-02-01T00:00:00Z")
            .await?;

        let err = repo
            .update_user(
                &ctx,
                "u-1",
                UpdateUserInput {
                    first_name: Some("Renamed".into()),
                    updated_at: "2024-03-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await;
        // Default-scope update treats soft-deleted as absent.
        assert!(matches!(err, Err(RepoError::NotFound(_))));
        Ok(())
    }
}

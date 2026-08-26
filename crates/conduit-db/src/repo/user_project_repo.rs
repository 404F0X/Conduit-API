//! UserProject repository — the `user_projects` join table (owner/member
//! assignment) that Go writes inside `ProjectService.CreateProject`
//! (`conduit/internal/server/biz/project.go`):
//!
//! ```text
//! client.UserProject.Create().
//!     SetUserID(currentUser.ID).
//!     SetProjectID(proj.ID).
//!     SetIsOwner(true).
//!     SetScopes([]string{}).
//!     Save(ctx)
//! ```
//!
//! ## Scope
//! Only the *create* surface is needed today (owner↔project assignment during
//! `SystemService::initialize` and, later, `ProjectService::create_project`).
//! The Go `UserProject` schema (`internal/ent/schema/user_project.go`) has
//! `TimeMixin` only — **no SoftDeleteMixin**, so there is no `deleted_at`
//! column and no `*_with_deleted` surface. `(user_id, project_id)` is unique
//! (`user_projects_by_user_id_project_id`); a duplicate insert surfaces the
//! backend's unique-constraint violation.
//!
//! The listing/update/delete surface that `UserService.{Add,Remove,Update}
//! ProjectUser` needs is a separate future task (see the pure plans in
//! `conduit-services::user_service`); this file intentionally ports only the
//! create path required to close the `myProjects` bootstrap gap.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_repo_principal};
use crate::row::UserProjectRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Fields a caller may set when assigning a user to a project. Mirrors the Go
/// `client.UserProject.Create()` builder used by `ProjectService.CreateProject`
/// — `user_id` / `project_id` (immutable edges), `is_owner` (default false,
/// mutable), and per-user project `scopes` (default `[]`).
#[derive(Debug, Clone)]
pub struct CreateUserProjectInput {
    pub id: String,
    pub user_id: String,
    pub project_id: String,
    pub is_owner: bool,
    pub scopes: Vec<String>,
    /// Caller-supplied timestamp (epoch millis or ISO-8601). Parsed into
    /// `DateTime<Utc>` on the row.
    pub created_at: String,
}

// --- timestamp parsing (mirrors project_repo.rs::parse_dt) -----------------

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

fn row_from_input(input: &CreateUserProjectInput) -> UserProjectRow {
    let now = parse_dt(&input.created_at);
    UserProjectRow {
        id: input.id.clone(),
        user_id: input.user_id.clone(),
        project_id: input.project_id.clone(),
        is_owner: input.is_owner,
        scopes: input.scopes.clone(),
        created_at: now,
        updated_at: now,
    }
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait UserProjectRepo: Send + Sync {
    /// Insert a `user_projects` row. Mirrors Go
    /// `client.UserProject.Create().Set*().Save(ctx)`. A duplicate
    /// `(user_id, project_id)` surfaces `RepoError::NameConflict` (the closest
    /// existing variant for a unique-index violation).
    async fn create_user_project_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateUserProjectInput,
    ) -> RepoResult<UserProjectRow>;

    async fn create_user_project(
        &self,
        ctx: &RequestContext,
        input: CreateUserProjectInput,
    ) -> RepoResult<UserProjectRow> {
        guard_repo_principal(ctx)?;
        self.create_user_project_unchecked(ctx, input).await
    }

    /// List the memberships for a given user (Go `Edges.ProjectUsers`). Ordered
    /// by `id ASC` for deterministic output. Used by tests and future callers.
    async fn list_user_projects_by_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<UserProjectRow>>;

    async fn list_user_projects_by_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<UserProjectRow>> {
        guard_repo_principal(ctx)?;
        self.list_user_projects_by_user_unchecked(ctx, user_id)
            .await
    }
}

// --- in-memory impl --------------------------------------------------------

/// In-memory `UserProjectRepo` for unit tests and the bootstrap runtime. Keyed
/// by `UserProjectRow.id`; enforces the `(user_id, project_id)` uniqueness the
/// Go index guarantees.
#[derive(Debug, Default)]
pub struct InMemoryUserProjectRepo {
    rows: Mutex<BTreeMap<String, UserProjectRow>>,
}

impl InMemoryUserProjectRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = UserProjectRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl UserProjectRepo for InMemoryUserProjectRepo {
    async fn create_user_project_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateUserProjectInput,
    ) -> RepoResult<UserProjectRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project repo"))?;
        // Uniqueness on (user_id, project_id) — mirrors the Go unique index
        // `user_projects_by_user_id_project_id`.
        let duplicate = guard
            .values()
            .any(|r| r.user_id == input.user_id && r.project_id == input.project_id);
        if duplicate {
            return Err(RepoError::NameConflict);
        }
        if guard.contains_key(&input.id) {
            return Err(RepoError::NotFound("user project id already present"));
        }
        let row = row_from_input(&input);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn list_user_projects_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<UserProjectRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project repo"))?;
        let mut rows: Vec<UserProjectRow> = guard
            .values()
            .filter(|r| r.user_id == user_id)
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn input(id: &str, user_id: &str, project_id: &str, is_owner: bool) -> CreateUserProjectInput {
        CreateUserProjectInput {
            id: id.to_string(),
            user_id: user_id.to_string(),
            project_id: project_id.to_string(),
            is_owner,
            scopes: vec![],
            created_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn create_then_list_by_user() -> Result<(), Box<dyn std::error::Error>> {
        let repo = InMemoryUserProjectRepo::new();
        let c = ctx();

        let row = repo
            .create_user_project(&c, input("up-1", "1", "10", true))
            .await?;
        assert_eq!(row.user_id, "1");
        assert_eq!(row.project_id, "10");
        assert!(row.is_owner);
        assert!(row.scopes.is_empty());

        let listed = repo.list_user_projects_by_user(&c, "1").await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].project_id, "10");
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_user_project_pair_conflicts() -> Result<(), Box<dyn std::error::Error>> {
        let repo = InMemoryUserProjectRepo::new();
        let c = ctx();
        repo.create_user_project(&c, input("up-1", "1", "10", true))
            .await?;
        let dup = repo
            .create_user_project(&c, input("up-2", "1", "10", false))
            .await;
        assert!(matches!(dup, Err(RepoError::NameConflict)));
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous() {
        let repo = InMemoryUserProjectRepo::new();
        let anon = RequestContext::new(PolicyContext::anonymous());
        let denied = repo
            .create_user_project(&anon, input("up-1", "1", "10", true))
            .await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));
    }
}

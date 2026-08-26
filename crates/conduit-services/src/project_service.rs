//! ProjectService — repo-backed port of `conduit/internal/server/biz/project.go`
//! (RUST-P5-003 S03).
//!
//! Go service surface (biz/project.go) → Rust mapping:
//!   * `CreateProject`          → [`ProjectService::create_project`]        (project.go:53-155)
//!   * `UpdateProject`          → [`ProjectService::update_project`]        (project.go:158-186)
//!   * `GetProjectByID`         → [`ProjectService::get_project_by_id`]     (project.go:188-232)
//!   * `UpdateProjectProfiles`  → [`ProjectService::update_project_profiles`] (project.go:235-254)
//!   * `ValidateProjectProfiles`→ re-exported [`validate_project_profiles`] (project.go:257-296;
//!     canonical Rust impl lives in `user_project_service.rs` — NOT duplicated here)
//!   * `UpdateProjectStatus`    → [`ProjectService::update_project_status`] (project.go:299-313)
//!   * `buildProjectCacheKey`   → [`build_project_cache_key`]               (project.go:315-317)
//!   * `invalidateProjectCache` → `ProjectService::invalidate_project_cache` (project.go:320-323)
//!   * `DeleteProject`          → [`ProjectService::delete_project`]        (project.go:333-383)
//!   * `PermissionValidator.CanDeleteProject` → [`can_delete_project`]
//!     (permission_validator.go:229-243)
//!
//! ## Division of labor with `user_project_service.rs`
//! `user_project_service.rs` holds the **pure** project-domain logic ported
//! earlier (create/soft-delete step plans, `validate_project_profiles`,
//! `ProjectStatus`). This module is the **repo-backed executor**: it bridges
//! those plans onto the `conduit-db` repo traits instead of re-implementing
//! the ordering. See `create_project` / `delete_project`, which literally
//! iterate `create_project_plan` / `soft_delete_project_plan` steps.
//!
//! ## Storage & transaction notes
//! Go wraps `DeleteProject` in `RunInTransaction` (project.go:339). The Rust
//! repo traits have no transaction surface yet (same situation documented on
//! `SystemService::initialize`); steps run in the Go-defined order and fail
//! fast. Go's `Role`/`APIKey` schemas carry `SoftDeleteMixin`, so the ent
//! `Delete()` calls in `DeleteProject` are soft deletes → mapped to
//! `soft_delete_role`/`soft_delete_api_key`. `UserProject` has only
//! `TimeMixin` (schema/user_project.go:17-21) → hard delete.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use conduit_auth::{Principal, PrincipalKind};
use conduit_cache::Cache;
use conduit_core::objects::project::ProjectProfiles;
use conduit_db::repo::role_repo::LEVEL_PROJECT;
use conduit_db::{
    ApiKeyRepo, CreateProjectInput as RepoCreateProjectInput, CreateRoleInput, ListApiKeysQuery,
    ProjectRepo, ProjectRow, RepoError, RepoResult, RequestContext, RoleRepo,
    UpdateProjectInput as RepoUpdateProjectInput,
};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::user_project_service::{
    self, CreateProjectStep, SoftDeleteStep, UserProjectServiceError,
};

// Canonical pure logic shared with the earlier RUST-P5-003 port — re-exported
// so callers of this module see the full Go `biz/project.go` surface in one
// place without a duplicate implementation.
pub use crate::user_project_service::{ProjectStatus, validate_project_profiles};

/// Go `negativeCacheTTL = 5 * time.Second` (project.go:25).
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(5);

/// Page size used when sweeping a project's API keys during delete. Go deletes
/// with a single bulk `Delete().Where(...)` (project.go:365-367); the Rust repo
/// exposes list + per-row soft delete, so we page through the live rows.
const DELETE_SWEEP_PAGE_SIZE: u32 = 100;

pub type ProjectServiceResult<T> = Result<T, ProjectServiceError>;

#[derive(Debug, Error)]
pub enum ProjectServiceError {
    #[error(transparent)]
    Repo(#[from] RepoError),
    /// Errors surfaced by the shared pure validators/plans in
    /// `user_project_service.rs` (`validate_project_profiles`,
    /// `create_project_plan`). Passed through transparently so callers see the
    /// Go-parity messages (e.g. `"duplicate profile name: X"`).
    #[error(transparent)]
    Validation(#[from] UserProjectServiceError),
    /// JSON encoding of the `profiles` column. Go serializes through ent and
    /// cannot fail here; kept as an explicit variant instead of a panic.
    #[error("failed to encode project profiles: {0}")]
    ProfilesEncoding(#[from] serde_json::Error),
    /// Go `fmt.Errorf("user not found in context")` (project.go:56;
    /// permission_validator.go:234).
    #[error("user not found in context")]
    UserNotFoundInContext,
    /// Go `xerrors.DuplicateNameError("project", input.Name)` (project.go:70).
    /// Message shape mirrors `xerrors/graphql.go:104-114`:
    /// `"%s name '%s' already exists"`.
    #[error("project name '{0}' already exists")]
    DuplicateName(String),
    /// Go `fmt.Errorf("failed to get project: %w (id: %d)", ErrProjectNotFound, id)`
    /// (project.go:195/218) with `ErrProjectNotFound = errors.New("project not
    /// found")` (biz/errors.go:22).
    #[error("failed to get project: project not found (id: {0})")]
    ProjectNotFound(String),
    /// Go `fmt.Errorf("insufficient permissions: only system owners can delete
    /// projects")` (permission_validator.go:239).
    #[error("insufficient permissions: only system owners can delete projects")]
    NotSystemOwner,
    /// Go `fmt.Errorf("permission denied: %w", err)` (project.go:336).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

// ---------------------------------------------------------------------------
// Inputs — mirror the ent-generated GraphQL inputs consumed by biz/project.go.
// ---------------------------------------------------------------------------

/// Mirrors Go `ent.CreateProjectInput` (gql_mutation_input.go:675-680) as
/// consumed by `biz.CreateProject`.
///
/// The ent input also carries `Status *project.Status` and `UserIDs []int`,
/// but `biz.CreateProject` never applies them — it only calls
/// `SetName`/`SetDescription` (project.go:74-79) — so they are intentionally
/// omitted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateProjectParams {
    pub name: String,
    /// Go `Description *string`: `None` = leave the column at its default.
    pub description: Option<String>,
}

/// Mirrors Go `ent.UpdateProjectInput` (gql_mutation_input.go:703-710) as
/// consumed by `biz.UpdateProject` (project.go:158-176).
///
/// The ent input also carries `Status *project.Status`, but `biz.UpdateProject`
/// never applies it (status changes go through `UpdateProjectStatus`), so it is
/// intentionally omitted here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateProjectParams {
    /// Go `SetNillableName` (project.go:162): `None` = no change.
    pub name: Option<String>,
    /// Go `SetNillableDescription` (project.go:163): `None` = no change.
    pub description: Option<String>,
    /// Go `if input.ClearUsers { mut.ClearUsers() }` (project.go:165-167).
    pub clear_users: bool,
    /// Go `mut.AddUserIDs(input.AddUserIDs...)` (project.go:169-171).
    pub add_user_ids: Vec<String>,
    /// Go `mut.RemoveUserIDs(input.RemoveUserIDs...)` (project.go:173-175).
    pub remove_user_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// user_projects link repo — no conduit-db repo exists for the `user_projects`
// table yet (SystemService::initialize records the same gap), so the service
// defines the minimal trait it needs. Policy enforcement note: every service
// flow touches a guarded conduit-db repo method (`project_exists`,
// `update_project`, `find_project`) *before* any link mutation, so anonymous
// callers are rejected at the conduit-db boundary and this trait stays lean.
// ---------------------------------------------------------------------------

/// One `user_projects` row. Mirrors Go ent `UserProject`
/// (schema/user_project.go:34-49): `user_id`, `project_id`, `is_owner`
/// (default `false`), `scopes` (default `[]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProjectLink {
    pub user_id: String,
    pub project_id: String,
    pub is_owner: bool,
    pub scopes: Vec<String>,
}

#[async_trait]
pub trait UserProjectLinkRepo: Send + Sync {
    /// Insert one membership row. Mirrors Go
    /// `client.UserProject.Create().SetUserID(..).SetProjectID(..).SetIsOwner(..).SetScopes(..)`
    /// (project.go:144-149). Errors with [`RepoError::NameConflict`] when the
    /// `(user_id, project_id)` pair already exists — the closest existing
    /// variant for the `user_projects_by_user_id_project_id` unique index
    /// (schema/user_project.go:25-27).
    async fn link_user(
        &self,
        ctx: &RequestContext,
        link: UserProjectLink,
    ) -> RepoResult<UserProjectLink>;

    /// Add plain membership rows with the ent column defaults
    /// (`is_owner=false`, `scopes=[]`). Mirrors Go `mut.AddUserIDs(...)`
    /// (project.go:169-171).
    async fn add_project_users(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        user_ids: &[String],
    ) -> RepoResult<()>;

    /// Remove specific memberships. Mirrors Go `mut.RemoveUserIDs(...)`
    /// (project.go:173-175). Removing a non-linked user is a no-op (ent edge
    /// delete-where semantics).
    async fn remove_project_users(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        user_ids: &[String],
    ) -> RepoResult<()>;

    /// Delete every membership row of a project. Backs both Go call sites with
    /// the same SQL effect: `mut.ClearUsers()` (project.go:165-167) and
    /// `client.UserProject.Delete().Where(userproject.ProjectIDEQ(id))`
    /// (project.go:349-351). Returns the number of rows removed.
    async fn delete_links_by_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<u64>;

    /// List a project's membership rows (test/inspection surface).
    async fn list_links_by_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<UserProjectLink>>;
}

/// In-memory [`UserProjectLinkRepo`] keyed by `(project_id, user_id)` —
/// mirrors the `user_projects_by_user_id_project_id` unique index.
#[derive(Debug, Default)]
pub struct InMemoryUserProjectLinkRepo {
    rows: Mutex<BTreeMap<(String, String), UserProjectLink>>,
}

impl InMemoryUserProjectLinkRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project link repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl UserProjectLinkRepo for InMemoryUserProjectLinkRepo {
    async fn link_user(
        &self,
        _ctx: &RequestContext,
        link: UserProjectLink,
    ) -> RepoResult<UserProjectLink> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project link repo"))?;
        let key = (link.project_id.clone(), link.user_id.clone());
        if guard.contains_key(&key) {
            // (user_id, project_id) unique index (schema/user_project.go:25-27).
            return Err(RepoError::NameConflict);
        }
        guard.insert(key, link.clone());
        Ok(link)
    }

    async fn add_project_users(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        user_ids: &[String],
    ) -> RepoResult<()> {
        for user_id in user_ids {
            // ent AddUserIDs creates rows with the column defaults:
            // is_owner=false (schema:40-41), scopes=[] (schema:45-48).
            self.link_user(
                ctx,
                UserProjectLink {
                    user_id: user_id.clone(),
                    project_id: project_id.to_string(),
                    is_owner: false,
                    scopes: Vec::new(),
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn remove_project_users(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        user_ids: &[String],
    ) -> RepoResult<()> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project link repo"))?;
        for user_id in user_ids {
            guard.remove(&(project_id.to_string(), user_id.clone()));
        }
        Ok(())
    }

    async fn delete_links_by_project(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<u64> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project link repo"))?;
        let before = guard.len();
        guard.retain(|(pid, _), _| pid != project_id);
        Ok((before - guard.len()) as u64)
    }

    async fn list_links_by_project(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<UserProjectLink>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("user project link repo"))?
            .iter()
            .filter(|((pid, _), _)| pid == project_id)
            .map(|(_, link)| link.clone())
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Cache entry — mirrors Go `xcache.Entry[ent.Project]`.
// ---------------------------------------------------------------------------

/// Mirrors Go `xcache.Entry[ent.Project]` (IsEmpty flag + value). Expiry is
/// delegated to the [`Cache`] backend TTL, so no `ExpiresAt` field is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectCacheEntry {
    #[serde(default)]
    is_empty: bool,
    #[serde(default)]
    value: Option<ProjectRow>,
}

/// Mirrors Go `buildProjectCacheKey` (project.go:315-317):
/// `fmt.Sprintf("project:%d", id)`.
pub fn build_project_cache_key(project_id: &str) -> String {
    format!("project:{project_id}")
}

// ---------------------------------------------------------------------------
// Permission guard — mirrors PermissionValidator.CanDeleteProject.
// ---------------------------------------------------------------------------

/// Mirrors Go `PermissionValidator.CanDeleteProject`
/// (permission_validator.go:229-243): only system owners can delete projects.
///
/// The `contexts.GetUser(ctx)` miss (permission_validator.go:232-235) maps to a
/// `User` principal without an id, the same convention as
/// `role_service::prevent_owner_escalation`. No kind-based bypass is added:
/// `Principal::system()`/`test()` pass because they carry `is_owner = true` —
/// the exact field Go reads (`currentUser.IsOwner`).
pub fn can_delete_project(actor: &Principal) -> ProjectServiceResult<()> {
    if actor.kind == PrincipalKind::User && actor.id.is_none() {
        return Err(ProjectServiceError::UserNotFoundInContext);
    }
    if !actor.is_owner {
        return Err(ProjectServiceError::NotSystemOwner);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Repo-backed `ProjectService`. Mirrors the Go struct (project.go:34-39):
/// `AbstractService.db` → the injected repos, `ProjectCache` → [`Cache`],
/// `permissionValidator` → the free fn [`can_delete_project`].
pub struct ProjectService {
    project_repo: Arc<dyn ProjectRepo>,
    role_repo: Arc<dyn RoleRepo>,
    api_key_repo: Arc<dyn ApiKeyRepo>,
    user_project_repo: Arc<dyn UserProjectLinkRepo>,
    cache: Arc<dyn Cache>,
}

impl ProjectService {
    /// Mirrors Go `NewProjectService` (project.go:41-49).
    pub fn new(
        project_repo: Arc<dyn ProjectRepo>,
        role_repo: Arc<dyn RoleRepo>,
        api_key_repo: Arc<dyn ApiKeyRepo>,
        user_project_repo: Arc<dyn UserProjectLinkRepo>,
        cache: Arc<dyn Cache>,
    ) -> Self {
        Self {
            project_repo,
            role_repo,
            api_key_repo,
            user_project_repo,
            cache,
        }
    }

    /// Mirrors Go `CreateProject` (project.go:53-155): duplicate-name check →
    /// project row → three default project-level roles (Admin/Developer/Viewer)
    /// → creator linked as owner.
    ///
    /// The side-effect ordering is driven by the shared pure plan
    /// `user_project_service::create_project_plan` — this method only executes
    /// the steps against the repos, so the ordering contract lives in one place.
    pub async fn create_project(
        &self,
        ctx: &RequestContext,
        actor: &Principal,
        params: CreateProjectParams,
    ) -> ProjectServiceResult<ProjectRow> {
        // Go project.go:54-57 — `contexts.GetUser(ctx)` must yield a user.
        let owner_user_id = actor
            .id
            .clone()
            .ok_or(ProjectServiceError::UserNotFoundInContext)?;

        // Go project.go:61-71 — duplicate name check over live rows (the ent
        // soft-delete mixin filters deleted rows; `project_exists` matches).
        if self.project_repo.project_exists(ctx, &params.name).await? {
            return Err(ProjectServiceError::DuplicateName(params.name));
        }

        // Bridge to the shared plan (project row → Admin → Developer → Viewer
        // → owner link). Ordering mirrors Go project.go:73-152.
        let plan =
            user_project_service::create_project_plan(&user_project_service::CreateProjectInput {
                name: params.name.clone(),
                description: params.description.clone(),
                owner_user_id,
            })?;

        let now = now_rfc3339();
        let mut created: Option<ProjectRow> = None;
        for step in plan.steps {
            match step {
                // Go project.go:73-84.
                CreateProjectStep::InsertProject { name, description } => {
                    let row = self
                        .project_repo
                        .create_project(
                            ctx,
                            RepoCreateProjectInput {
                                id: generate_id("project"),
                                name,
                                description,
                                created_at: now.clone(),
                            },
                        )
                        .await?;
                    created = Some(row);
                }
                // Go project.go:86-141 — role.LevelProject + the project id.
                CreateProjectStep::SeedDefaultRole { role_name, scopes } => {
                    let project_id = created
                        .as_ref()
                        .map(|row| row.id.clone())
                        .ok_or(RepoError::NotFound("project row missing before role seed"))?;
                    self.role_repo
                        .create_role(
                            ctx,
                            CreateRoleInput {
                                id: generate_id("role"),
                                name: role_name.to_string(),
                                level: LEVEL_PROJECT.to_string(),
                                project_id,
                                scopes: scopes.iter().map(|s| (*s).to_string()).collect(),
                                created_at: now.clone(),
                            },
                        )
                        .await?;
                }
                // Go project.go:143-152 — IsOwner=true, Scopes=[].
                CreateProjectStep::LinkOwner { user_id } => {
                    let project_id = created
                        .as_ref()
                        .map(|row| row.id.clone())
                        .ok_or(RepoError::NotFound("project row missing before owner link"))?;
                    self.user_project_repo
                        .link_user(
                            ctx,
                            UserProjectLink {
                                user_id,
                                project_id,
                                is_owner: true,
                                scopes: Vec::new(),
                            },
                        )
                        .await?;
                }
                // Never emitted by `create_project_plan`.
                CreateProjectStep::SoftDelete(_) => {}
            }
        }

        created.ok_or(ProjectServiceError::Repo(RepoError::NotFound(
            "create plan produced no project row",
        )))
    }

    /// Mirrors Go `UpdateProject` (project.go:158-186).
    pub async fn update_project(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        params: UpdateProjectParams,
    ) -> ProjectServiceResult<ProjectRow> {
        // Go project.go:161-163 — SetNillableName / SetNillableDescription.
        // `status`/`profiles` stay untouched (biz.UpdateProject never sets them).
        let row = self
            .project_repo
            .update_project(
                ctx,
                project_id,
                RepoUpdateProjectInput {
                    name: params.name.clone(),
                    description: params.description.clone().map(Some),
                    status: None,
                    profiles: None,
                    updated_at: now_rfc3339(),
                },
            )
            .await?;

        // Edge mutations in Go order: ClearUsers → AddUserIDs → RemoveUserIDs
        // (project.go:165-175). Go checks `!= nil`; an empty Vec adds/removes
        // nothing, which is behaviorally identical.
        if params.clear_users {
            self.user_project_repo
                .delete_links_by_project(ctx, project_id)
                .await?;
        }
        if !params.add_user_ids.is_empty() {
            self.user_project_repo
                .add_project_users(ctx, project_id, &params.add_user_ids)
                .await?;
        }
        if !params.remove_user_ids.is_empty() {
            self.user_project_repo
                .remove_project_users(ctx, project_id, &params.remove_user_ids)
                .await?;
        }

        // Go project.go:183.
        self.invalidate_project_cache(project_id).await;

        Ok(row)
    }

    /// Mirrors Go `GetProjectByID` (project.go:188-232): cache lookup with a
    /// negative-entry guard, then repo fetch, then positive/negative caching.
    pub async fn get_project_by_id(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> ProjectServiceResult<ProjectRow> {
        let cache_key = build_project_cache_key(project_id);

        // Go project.go:192-204 — cache errors are swallowed (only the hit
        // path exists); TTL expiry is handled by the backend.
        if let Ok(Some(value)) = self.cache.get(&cache_key).await
            && let Ok(entry) = serde_json::from_value::<ProjectCacheEntry>(value)
        {
            if entry.is_empty {
                // Negative entry (Go project.go:194-196).
                return Err(ProjectServiceError::ProjectNotFound(project_id.to_string()));
            }
            // Stale-entry guard (Go project.go:198-202): only trust
            // entries carrying a real row id.
            if let Some(row) = entry.value
                && !row.id.is_empty()
            {
                return Ok(row);
            }
        }

        // Go project.go:206-209 ("ent client not found in context") has no
        // Rust analogue: the repo is injected at construction.

        match self.project_repo.find_project(ctx, project_id).await? {
            None => {
                // Negative caching to prevent cache penetration
                // (Go project.go:213-219, TTL = negativeCacheTTL).
                let entry = ProjectCacheEntry {
                    is_empty: true,
                    value: None,
                };
                if let Ok(value) = serde_json::to_value(&entry) {
                    let _ = self
                        .cache
                        .set(&cache_key, value, Some(NEGATIVE_CACHE_TTL))
                        .await;
                }
                Err(ProjectServiceError::ProjectNotFound(project_id.to_string()))
            }
            Some(row) => {
                // Positive caching with the backend default TTL (Go
                // project.go:223-229; a set failure is only logged in Go —
                // ignored here).
                let entry = ProjectCacheEntry {
                    is_empty: false,
                    value: Some(row.clone()),
                };
                if let Ok(value) = serde_json::to_value(&entry) {
                    let _ = self.cache.set(&cache_key, value, None).await;
                }
                Ok(row)
            }
        }
    }

    /// Mirrors Go `UpdateProjectProfiles` (project.go:235-254): validate via
    /// the shared `ValidateProjectProfiles` port, persist the JSON column,
    /// invalidate the cache.
    pub async fn update_project_profiles(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        profiles: &ProjectProfiles,
    ) -> ProjectServiceResult<ProjectRow> {
        // Go project.go:237-239 → ValidateProjectProfiles (project.go:257-296).
        validate_project_profiles(profiles)?;

        let row = self
            .project_repo
            .update_project(
                ctx,
                project_id,
                RepoUpdateProjectInput {
                    profiles: Some(serde_json::to_value(profiles)?),
                    updated_at: now_rfc3339(),
                    ..RepoUpdateProjectInput::default()
                },
            )
            .await?;

        // Go project.go:250-251.
        self.invalidate_project_cache(project_id).await;

        Ok(row)
    }

    /// Mirrors Go `UpdateProjectStatus` (project.go:299-313).
    pub async fn update_project_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        status: ProjectStatus,
    ) -> ProjectServiceResult<ProjectRow> {
        // Go stores the enum's string form (`active`/`archived`); matches the
        // `ProjectStatus` lowercase serde contract in user_project_service.rs.
        let status_str = match status {
            ProjectStatus::Active => "active",
            ProjectStatus::Archived => "archived",
        };

        let row = self
            .project_repo
            .update_project(
                ctx,
                project_id,
                RepoUpdateProjectInput {
                    status: Some(status_str.to_string()),
                    updated_at: now_rfc3339(),
                    ..RepoUpdateProjectInput::default()
                },
            )
            .await?;

        // Go project.go:309-310.
        self.invalidate_project_cache(project_id).await;

        Ok(row)
    }

    /// Mirrors Go `DeleteProject` (project.go:333-383):
    /// 1. permission check (`CanDeleteProject`)
    /// 2. delete `user_projects` rows (hard — no SoftDeleteMixin)
    /// 3. soft-delete project-level roles
    /// 4. soft-delete project API keys
    /// 5. soft-delete the project row
    /// 6. invalidate the project cache
    ///
    /// The cascade ordering is driven by the shared pure plan
    /// `user_project_service::soft_delete_project_plan`. Go wraps steps 2-5 in
    /// `RunInTransaction`; the repo traits have no transaction surface yet
    /// (same note as `SystemService::initialize`), so the steps run in order
    /// and fail fast.
    pub async fn delete_project(
        &self,
        ctx: &RequestContext,
        actor: &Principal,
        project_id: &str,
    ) -> ProjectServiceResult<()> {
        // Go project.go:334-337 — wrap as "permission denied: %w".
        can_delete_project(actor)
            .map_err(|err| ProjectServiceError::PermissionDenied(err.to_string()))?;

        // Go project.go:342-346 — verify the project exists.
        let project = self
            .project_repo
            .find_project(ctx, project_id)
            .await?
            .ok_or_else(|| ProjectServiceError::ProjectNotFound(project_id.to_string()))?;

        let now = now_rfc3339();
        for step in user_project_service::soft_delete_project_plan().steps {
            let CreateProjectStep::SoftDelete(step) = step else {
                continue; // The soft-delete plan only emits SoftDelete steps.
            };
            match step {
                // Go project.go:348-354 — UserProject has no soft delete.
                SoftDeleteStep::DeleteProjectUsers => {
                    self.user_project_repo
                        .delete_links_by_project(ctx, &project.id)
                        .await?;
                }
                // Go project.go:356-362 — Role carries SoftDeleteMixin, so
                // ent's bulk Delete() is a soft delete.
                SoftDeleteStep::DeleteProjectRoles => {
                    for role in self
                        .role_repo
                        .list_roles_by_project(ctx, &project.id)
                        .await?
                    {
                        self.role_repo.soft_delete_role(ctx, &role.id, &now).await?;
                    }
                }
                // Go project.go:364-370 — APIKey also soft-deletes. Page from
                // offset 0 each pass: soft-deleted rows leave the live set, so
                // re-fetching the first page never skips survivors.
                SoftDeleteStep::DeleteProjectApiKeys => loop {
                    let query = ListApiKeysQuery {
                        project_id: Some(project.id.clone()),
                        limit: DELETE_SWEEP_PAGE_SIZE,
                        ..ListApiKeysQuery::default()
                    };
                    let page = self
                        .api_key_repo
                        .list_api_keys_by_project(ctx, &project.id, &query)
                        .await?;
                    if page.rows.is_empty() {
                        break;
                    }
                    for key in &page.rows {
                        self.api_key_repo
                            .soft_delete_api_key(ctx, &key.id, &now)
                            .await?;
                    }
                    if !page.has_more {
                        break;
                    }
                },
                // Go project.go:372-376.
                SoftDeleteStep::SoftDeleteProject => {
                    self.project_repo
                        .soft_delete_project(ctx, &project.id, &now)
                        .await?;
                }
            }
        }

        // Go project.go:378-379.
        self.invalidate_project_cache(project_id).await;

        Ok(())
    }

    /// Mirrors Go `invalidateProjectCache` (project.go:320-323); the delete
    /// error is swallowed (`_ = s.ProjectCache.Delete(...)`).
    async fn invalidate_project_cache(&self, project_id: &str) {
        let _ = self
            .cache
            .delete(&build_project_cache_key(project_id))
            .await;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Rust stand-in for Go's ent `time.Now()` column defaults.
fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// Random row id (`{prefix}_{hex}`), following the `SystemService::initialize`
/// convention. Go relies on ent's autoincrement ids; the Rust repos take
/// caller-supplied string ids.
fn generate_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut hex = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    format!("{prefix}_{hex}")
}

// ---------------------------------------------------------------------------
// Tests — mirror Go `biz/project_test.go` (336 lines, 5 top-level tests)
// golden cases plus the biz/project.go behaviors that Go covers only through
// the ent-backed integration paths.
//
// Go test → Rust test mapping (Mendel-the-8th audit, 2026-07-06):
//   * `TestProjectService_GetProjectByID` (project_test.go:33-72)
//       → `get_project_by_id_returns_row_and_not_found`
//   * `TestProjectService_GetProjectByID_WithDifferentCaches` (project_test.go:74-153)
//       → Memory + Noop sub-cases: `get_project_by_id_with_memory_and_noop_cache_sees_update`
//       → negative-caching branch (project.go:213-219, not explicitly tested in
//         Go project_test.go): `negative_cache_prevents_penetration_within_ttl`
//       → stale-entry guard (project.go:198-202): `stale_cache_entry_without_row_id_falls_through_to_db`
//   * `TestProjectService_UpdateProjectStatus_CacheInvalidation` (project_test.go:155-190)
//       → `update_project_status_invalidates_cache`
//   * `TestBuildProjectCacheKey` (project_test.go:192-221)
//       → `build_project_cache_key_golden`
//   * `TestValidateProjectProfiles` (project_test.go:223-336, 9 sub-cases)
//       → mirrored 1:1 in `user_project_service.rs` (9 tests); NOT duplicated here.
//
// Pending DB/xcache-backed sub-tests (standard treatment — see Mendel-the-5th
// `apikey_service.rs::go_api_key_test_pending_db_backed_subtests_catalogue`
// for the precedent; Go uses an Ent test database + `miniredis.RunT`):
//   * `TestProjectService_GetProjectByID_WithDifferentCaches/Redis Cache`
//     (project_test.go:84-91) — requires a miniredis-equivalent in-process
//     Redis; the Rust `RedisCache` backend needs a live Redis connection.
//   * `TestProjectService_GetProjectByID_WithDifferentCaches/Two-Level Cache`
//     (project_test.go:92-100) — same miniredis dependency via `TwoLevelCache`'s
//     remote tier.
//   * Go scaffolding difference (not a gap): Go creates projects directly via
//     `client.Project.Create()` (ent auto-increment int IDs), Rust tests create
//     via `service.create_project` (string IDs). The assertion intent (row
//     retrievable by ID, cache invalidation on update) is mirrored; the
//     ent-direct-create path is approximated by the service-level create.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::{MemoryCache, NoopCache};
    use conduit_db::{
        CreateApiKeyInput, InMemoryApiKeyRepo, InMemoryProjectRepo, InMemoryRoleRepo,
        PolicyContext, Principal as DbPrincipal,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    struct Harness {
        service: ProjectService,
        project_repo: Arc<InMemoryProjectRepo>,
        role_repo: Arc<InMemoryRoleRepo>,
        api_key_repo: Arc<InMemoryApiKeyRepo>,
        link_repo: Arc<InMemoryUserProjectLinkRepo>,
    }

    fn harness_with_cache(cache: Arc<dyn Cache>) -> Harness {
        let project_repo = Arc::new(InMemoryProjectRepo::new());
        let role_repo = Arc::new(InMemoryRoleRepo::new());
        let api_key_repo = Arc::new(InMemoryApiKeyRepo::new());
        let link_repo = Arc::new(InMemoryUserProjectLinkRepo::new());
        let service = ProjectService::new(
            Arc::clone(&project_repo) as Arc<dyn ProjectRepo>,
            Arc::clone(&role_repo) as Arc<dyn RoleRepo>,
            Arc::clone(&api_key_repo) as Arc<dyn ApiKeyRepo>,
            Arc::clone(&link_repo) as Arc<dyn UserProjectLinkRepo>,
            cache,
        );
        Harness {
            service,
            project_repo,
            role_repo,
            api_key_repo,
            link_repo,
        }
    }

    fn harness() -> Harness {
        // Noop cache = Go's empty `xcache.Config{}` in setupTestProjectService
        // (project_test.go:21-31).
        harness_with_cache(Arc::new(NoopCache::new()))
    }

    fn memory_cache() -> Arc<dyn Cache> {
        Arc::new(MemoryCache::new(Duration::from_secs(300)))
    }

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(DbPrincipal::test()))
    }

    fn anon_ctx() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn owner_actor(id: &str) -> Principal {
        Principal::user(id).with_owner(true)
    }

    async fn create_named_project(
        h: &Harness,
        ctx: &RequestContext,
        name: &str,
    ) -> ProjectServiceResult<ProjectRow> {
        h.service
            .create_project(
                ctx,
                &owner_actor("user-1"),
                CreateProjectParams {
                    name: name.to_string(),
                    description: Some(name.to_string()),
                },
            )
            .await
    }

    // --- CreateProject (Go project.go:53-155) ------------------------------

    #[tokio::test]
    async fn create_project_creates_row_roles_and_owner_link() -> TestResult {
        let h = harness();
        let ctx = ctx();

        let project = create_named_project(&h, &ctx, "Alpha").await?;
        assert_eq!(project.name, "Alpha");
        assert_eq!(project.description, "Alpha");
        // Go project schema default status is `active`.
        assert_eq!(project.status, "active");

        // Three default project-level roles (Go project.go:86-141).
        let roles = h.role_repo.list_roles_by_project(&ctx, &project.id).await?;
        let mut names: Vec<&str> = roles.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Admin", "Developer", "Viewer"]);
        for role in &roles {
            assert_eq!(role.level, "project");
            assert_eq!(role.project_id, project.id);
            let expected: Vec<String> = match role.name.as_str() {
                // Go adminScopes (project.go:88-97).
                "Admin" => user_project_service::default_admin_role_scopes(),
                // Go developerScopes (project.go:110-115).
                "Developer" => user_project_service::default_developer_role_scopes(),
                // Go viewerScopes (project.go:128-131).
                _ => user_project_service::default_viewer_role_scopes(),
            }
            .iter()
            .map(|s| (*s).to_string())
            .collect();
            assert_eq!(role.scopes, expected, "scopes for {}", role.name);
        }

        // Creator linked as owner with empty scopes (Go project.go:143-152).
        let links = h.link_repo.list_links_by_project(&ctx, &project.id).await?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].user_id, "user-1");
        assert!(links[0].is_owner);
        assert!(links[0].scopes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn create_project_rejects_duplicate_name() -> TestResult {
        let h = harness();
        let ctx = ctx();
        create_named_project(&h, &ctx, "Alpha").await?;

        let dup = create_named_project(&h, &ctx, "Alpha").await;
        // Go: xerrors.DuplicateNameError("project", "Alpha") →
        // "project name 'Alpha' already exists" (xerrors/graphql.go:104-114).
        match dup {
            Err(ProjectServiceError::DuplicateName(name)) => {
                assert_eq!(name, "Alpha");
                assert_eq!(
                    ProjectServiceError::DuplicateName(name).to_string(),
                    "project name 'Alpha' already exists"
                );
            }
            other => panic!("expected DuplicateName, got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn create_project_requires_user_in_context() -> TestResult {
        let h = harness();
        let ctx = ctx();

        // Go project.go:54-57 — contexts.GetUser miss.
        let mut anon_actor = Principal::user("x");
        anon_actor.id = None;
        let denied = h
            .service
            .create_project(
                &ctx,
                &anon_actor,
                CreateProjectParams {
                    name: "Beta".to_string(),
                    description: None,
                },
            )
            .await;
        assert!(matches!(
            denied,
            Err(ProjectServiceError::UserNotFoundInContext)
        ));
        assert_eq!(h.project_repo.len()?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn anonymous_policy_context_is_denied_by_repo_guard() -> TestResult {
        let h = harness();

        // The conduit-db policy guard (guard_repo_principal) fires on the first
        // repo call (`project_exists`), mirroring the ent privacy layer.
        let denied = create_named_project(&h, &anon_ctx(), "Gamma").await;
        assert!(matches!(
            denied,
            Err(ProjectServiceError::Repo(RepoError::Policy(_)))
        ));
        Ok(())
    }

    // --- GetProjectByID (Go project_test.go:33-72) --------------------------

    #[tokio::test]
    async fn get_project_by_id_returns_row_and_not_found() -> TestResult {
        // Noop cache — mirrors TestProjectService_GetProjectByID's empty
        // xcache.Config{}.
        let h = harness();
        let ctx = ctx();
        let created = create_named_project(&h, &ctx, "Get-Me").await?;

        let fetched = h.service.get_project_by_id(&ctx, &created.id).await?;
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.name, created.name);
        assert_eq!(fetched.status, created.status);

        // Second call still works (noop cache) — project_test.go:63-66.
        let fetched2 = h.service.get_project_by_id(&ctx, &created.id).await?;
        assert_eq!(fetched2.id, created.id);

        // Invalid id — project_test.go:68-71 asserts the error message
        // contains "failed to get project".
        let missing = h.service.get_project_by_id(&ctx, "project_99999").await;
        match missing {
            Err(err) => assert!(
                err.to_string().contains("failed to get project"),
                "unexpected error: {err}"
            ),
            Ok(row) => panic!("expected error, got {row:?}"),
        }
        Ok(())
    }

    // --- GetProjectByID cache variants (Go project_test.go:74-153) ----------

    #[tokio::test]
    async fn get_project_by_id_with_memory_and_noop_cache_sees_update() -> TestResult {
        // Mirrors the Memory + Noop sub-cases of
        // TestProjectService_GetProjectByID_WithDifferentCaches; the Redis and
        // Two-Level variants need an external Redis (Go uses miniredis).
        let caches: Vec<Arc<dyn Cache>> = vec![memory_cache(), Arc::new(NoopCache::new())];
        for cache in caches {
            let h = harness_with_cache(cache);
            let ctx = ctx();
            let created = create_named_project(&h, &ctx, "Cached").await?;

            // First retrieval — hits the repo.
            let first = h.service.get_project_by_id(&ctx, &created.id).await?;
            assert_eq!(first.name, "Cached");
            // Second retrieval — hits the cache when enabled.
            let second = h.service.get_project_by_id(&ctx, &created.id).await?;
            assert_eq!(second.name, "Cached");

            // Update invalidates the cache (project_test.go:139-144).
            h.service
                .update_project(
                    &ctx,
                    &created.id,
                    UpdateProjectParams {
                        name: Some("Updated Cached".to_string()),
                        ..UpdateProjectParams::default()
                    },
                )
                .await?;

            // Third retrieval — must observe the new name (project_test.go:146-150).
            let third = h.service.get_project_by_id(&ctx, &created.id).await?;
            assert_eq!(third.name, "Updated Cached");
        }
        Ok(())
    }

    #[tokio::test]
    async fn negative_cache_prevents_penetration_within_ttl() -> TestResult {
        // Mirrors the negative-caching branch (Go project.go:213-219): a miss
        // is cached for negativeCacheTTL, so an immediately-following create
        // does not become visible through GetProjectByID within the window.
        let h = harness_with_cache(memory_cache());
        let ctx = ctx();

        let miss = h.service.get_project_by_id(&ctx, "p-ghost").await;
        assert!(matches!(miss, Err(ProjectServiceError::ProjectNotFound(_))));

        // Create the row directly through the repo with the same id.
        h.project_repo
            .create_project(
                &ctx,
                RepoCreateProjectInput {
                    id: "p-ghost".to_string(),
                    name: "Ghost".to_string(),
                    description: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                },
            )
            .await?;

        // Still a miss — served from the negative cache entry.
        let still_miss = h.service.get_project_by_id(&ctx, "p-ghost").await;
        assert!(matches!(
            still_miss,
            Err(ProjectServiceError::ProjectNotFound(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn stale_cache_entry_without_row_id_falls_through_to_db() -> TestResult {
        // Mirrors the stale-entry guard (Go project.go:198-202): an entry whose
        // value lacks a real id (e.g. written by an older version) is ignored.
        let cache = memory_cache();
        let h = harness_with_cache(Arc::clone(&cache));
        let ctx = ctx();
        let created = create_named_project(&h, &ctx, "Stale").await?;

        // Poison the cache with a non-empty entry whose row id is empty.
        let stale = serde_json::json!({ "isEmpty": false, "value": null });
        cache
            .set(&build_project_cache_key(&created.id), stale, None)
            .await?;

        let fetched = h.service.get_project_by_id(&ctx, &created.id).await?;
        assert_eq!(fetched.id, created.id);
        assert_eq!(fetched.name, "Stale");
        Ok(())
    }

    // --- UpdateProjectStatus cache invalidation (Go project_test.go:155-190) -

    #[tokio::test]
    async fn update_project_status_invalidates_cache() -> TestResult {
        let h = harness_with_cache(memory_cache());
        let ctx = ctx();
        let created = create_named_project(&h, &ctx, "Status").await?;

        // Populate the cache.
        let first = h.service.get_project_by_id(&ctx, &created.id).await?;
        assert_eq!(first.status, "active");

        // Update the status (project_test.go:182-184).
        h.service
            .update_project_status(&ctx, &created.id, ProjectStatus::Archived)
            .await?;

        // Second retrieval must see the archived status (cache invalidated).
        let second = h.service.get_project_by_id(&ctx, &created.id).await?;
        assert_eq!(second.status, "archived");
        Ok(())
    }

    // --- buildProjectCacheKey golden (Go project_test.go:192-221) -----------

    #[test]
    fn build_project_cache_key_golden() {
        assert_eq!(build_project_cache_key("1"), "project:1");
        assert_eq!(build_project_cache_key("123"), "project:123");
        assert_eq!(build_project_cache_key("999999"), "project:999999");
    }

    // --- UpdateProject user-edge mutations (Go project.go:165-175) ----------

    #[tokio::test]
    async fn update_project_applies_user_edge_mutations_in_go_order() -> TestResult {
        let h = harness();
        let ctx = ctx();
        let project = create_named_project(&h, &ctx, "Edges").await?;

        // AddUserIDs (project.go:169-171).
        h.service
            .update_project(
                &ctx,
                &project.id,
                UpdateProjectParams {
                    add_user_ids: vec!["user-2".to_string(), "user-3".to_string()],
                    ..UpdateProjectParams::default()
                },
            )
            .await?;
        let links = h.link_repo.list_links_by_project(&ctx, &project.id).await?;
        assert_eq!(links.len(), 3); // owner + user-2 + user-3
        let added = links
            .iter()
            .find(|l| l.user_id == "user-2")
            .ok_or("user-2 not linked")?;
        // ent AddUserIDs rows carry the schema defaults.
        assert!(!added.is_owner);
        assert!(added.scopes.is_empty());

        // RemoveUserIDs (project.go:173-175).
        h.service
            .update_project(
                &ctx,
                &project.id,
                UpdateProjectParams {
                    remove_user_ids: vec!["user-2".to_string()],
                    ..UpdateProjectParams::default()
                },
            )
            .await?;
        let links = h.link_repo.list_links_by_project(&ctx, &project.id).await?;
        assert_eq!(links.len(), 2);
        assert!(!links.iter().any(|l| l.user_id == "user-2"));

        // ClearUsers (project.go:165-167).
        h.service
            .update_project(
                &ctx,
                &project.id,
                UpdateProjectParams {
                    clear_users: true,
                    ..UpdateProjectParams::default()
                },
            )
            .await?;
        let links = h.link_repo.list_links_by_project(&ctx, &project.id).await?;
        assert!(links.is_empty());
        Ok(())
    }

    // --- UpdateProjectProfiles (Go project.go:235-254) -----------------------

    #[tokio::test]
    async fn update_project_profiles_validates_then_persists() -> TestResult {
        use conduit_core::objects::project::ProjectProfile;

        let h = harness_with_cache(memory_cache());
        let ctx = ctx();
        let project = create_named_project(&h, &ctx, "Profiles").await?;

        // Populate the cache with the pre-update row.
        let cached = h.service.get_project_by_id(&ctx, &project.id).await?;
        assert_eq!(cached.profiles, serde_json::json!({}));

        // Invalid profiles are rejected before any write
        // (Go project.go:237-239 → ValidateProjectProfiles).
        let invalid = ProjectProfiles {
            active_profile: String::new(),
            profiles: vec![
                ProjectProfile {
                    name: "P1".to_string(),
                    ..ProjectProfile::default()
                },
                ProjectProfile {
                    name: "p1".to_string(), // duplicate, case-insensitive
                    ..ProjectProfile::default()
                },
            ],
        };
        let rejected = h
            .service
            .update_project_profiles(&ctx, &project.id, &invalid)
            .await;
        match rejected {
            Err(ProjectServiceError::Validation(err)) => {
                // Go: "duplicate profile name: p1".
                assert_eq!(err.to_string(), "duplicate profile name: p1");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }

        // Valid profiles persist and invalidate the cache.
        let valid = ProjectProfiles {
            active_profile: "P1".to_string(),
            profiles: vec![ProjectProfile {
                name: "P1".to_string(),
                ..ProjectProfile::default()
            }],
        };
        let updated = h
            .service
            .update_project_profiles(&ctx, &project.id, &valid)
            .await?;
        assert_eq!(updated.profiles["activeProfile"], "P1");

        // Cache was invalidated: the next read observes the new profiles.
        let refetched = h.service.get_project_by_id(&ctx, &project.id).await?;
        assert_eq!(refetched.profiles["activeProfile"], "P1");
        Ok(())
    }

    // --- DeleteProject permission (permission_validator.go:229-243) ---------

    #[test]
    fn can_delete_project_only_for_system_owners() {
        // Owner passes (permission_validator.go:238-242).
        assert!(can_delete_project(&owner_actor("user-1")).is_ok());

        // Regular user is rejected (permission_validator.go:239).
        assert!(matches!(
            can_delete_project(&Principal::user("user-2")),
            Err(ProjectServiceError::NotSystemOwner)
        ));

        // Missing user in context (permission_validator.go:232-235).
        let mut anon = Principal::user("x");
        anon.id = None;
        assert!(matches!(
            can_delete_project(&anon),
            Err(ProjectServiceError::UserNotFoundInContext)
        ));
    }

    #[tokio::test]
    async fn delete_project_requires_system_owner() -> TestResult {
        let h = harness();
        let ctx = ctx();
        let project = create_named_project(&h, &ctx, "Guarded").await?;

        let denied = h
            .service
            .delete_project(&ctx, &Principal::user("user-2"), &project.id)
            .await;
        match denied {
            Err(ProjectServiceError::PermissionDenied(msg)) => {
                // Go project.go:336: "permission denied: %w" wrapping the
                // validator's message.
                assert_eq!(
                    ProjectServiceError::PermissionDenied(msg).to_string(),
                    "permission denied: insufficient permissions: only system owners can delete projects"
                );
            }
            other => panic!("expected PermissionDenied, got {other:?}"),
        }
        // Nothing was deleted.
        assert!(
            h.project_repo
                .find_project(&ctx, &project.id)
                .await?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_project_missing_project_errors() -> TestResult {
        let h = harness();
        let ctx = ctx();

        // Go project.go:342-346 — the get inside the transaction fails.
        let missing = h
            .service
            .delete_project(&ctx, &owner_actor("user-1"), "project_missing")
            .await;
        assert!(matches!(
            missing,
            Err(ProjectServiceError::ProjectNotFound(_))
        ));
        Ok(())
    }

    // --- DeleteProject cascade (Go project.go:333-383) -----------------------

    #[tokio::test]
    async fn delete_project_cascades_links_roles_api_keys_then_soft_deletes() -> TestResult {
        let h = harness_with_cache(memory_cache());
        let ctx = ctx();
        let project = create_named_project(&h, &ctx, "Doomed").await?;

        // Extra membership + an API key so every cascade branch has data.
        h.service
            .update_project(
                &ctx,
                &project.id,
                UpdateProjectParams {
                    add_user_ids: vec!["user-2".to_string()],
                    ..UpdateProjectParams::default()
                },
            )
            .await?;
        h.api_key_repo
            .create_api_key(
                &ctx,
                CreateApiKeyInput {
                    id: "ak-1".to_string(),
                    user_id: Some("user-1".to_string()),
                    project_id: project.id.clone(),
                    name: "default".to_string(),
                    key: "conduit-secret".to_string(),
                    key_type: "user".to_string(),
                    scopes: vec![],
                    profiles: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                },
            )
            .await?;

        // Warm the cache so the final invalidation is observable.
        h.service.get_project_by_id(&ctx, &project.id).await?;

        h.service
            .delete_project(&ctx, &owner_actor("user-1"), &project.id)
            .await?;

        // 1. UserProject rows hard-deleted (project.go:348-354).
        assert!(
            h.link_repo
                .list_links_by_project(&ctx, &project.id)
                .await?
                .is_empty()
        );
        // 2. Project roles soft-deleted (project.go:356-362).
        assert!(
            h.role_repo
                .list_roles_by_project(&ctx, &project.id)
                .await?
                .is_empty()
        );
        // 3. Project API keys soft-deleted (project.go:364-370).
        let keys = h
            .api_key_repo
            .list_api_keys_by_project(
                &ctx,
                &project.id,
                &ListApiKeysQuery::for_project(&project.id),
            )
            .await?;
        assert!(keys.rows.is_empty());
        assert!(
            h.api_key_repo
                .find_api_key_by_id(&ctx, "ak-1")
                .await?
                .is_none()
        );
        // 4. Project soft-deleted (project.go:372-376): hidden from live reads,
        //    still visible through the with-deleted surface.
        assert!(
            h.project_repo
                .find_project(&ctx, &project.id)
                .await?
                .is_none()
        );
        assert!(
            h.project_repo
                .find_project_with_deleted(&ctx, &project.id)
                .await?
                .is_some()
        );
        // 5. Cache invalidated (project.go:378-379): the read misses and takes
        //    the not-found path instead of serving the stale cached row.
        let gone = h.service.get_project_by_id(&ctx, &project.id).await;
        assert!(matches!(gone, Err(ProjectServiceError::ProjectNotFound(_))));
        Ok(())
    }
}
